//! Optional Linux-only, session-bound browser PTY. No helper or MCP shell.
use std::{collections::VecDeque, fs::File, io, os::fd::{AsRawFd,FromRawFd,OwnedFd},
    path::{Path,PathBuf}, process::Stdio, sync::{Arc,atomic::{AtomicBool,Ordering}},
    time::{Duration,Instant}};
use anyhow::{Result,ensure};
use axum::{Json,extract::{Extension,State},http::{HeaderMap,StatusCode,header},response::{IntoResponse,Response}};
use base64::{Engine,engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use tokio::{io::unix::AsyncFd,process::{Child,Command},sync::mpsc};
use uuid::Uuid;
use super::{linux_session_store::SessionKey,sessions::Sessions,tools::error};
use crate::{config::Platform,state::AppState};

const CAPACITY:usize=256*1024;
const CHUNK:usize=32*1024;
const IDLE:Duration=Duration::from_secs(45);
const LIFETIME:Duration=Duration::from_secs(1800);

pub(super) fn available(state:&AppState)->bool {
    state.config.platform==Platform::Linux && !state.config.test_mode
        && state.config.linux_authority.as_ref().is_some_and(|a|a.settings.terminal_enabled)
        && unsafe{libc::getuid()!=0 && libc::geteuid()==libc::getuid()}
        && Path::new("/bin/sh").is_file()
}

#[derive(Default)]
struct Output{bytes:VecDeque<u8>,end:u64}
impl Output {
    fn append(&mut self,data:&[u8]){
        self.end+=data.len() as u64;
        self.bytes.extend(data.iter().copied());
        let excess=self.bytes.len().saturating_sub(CAPACITY);
        self.bytes.drain(..excess);
    }
    fn read(&self,cursor:u64)->Result<(Vec<u8>,u64,bool)>{
        ensure!(cursor<=self.end,"invalid terminal cursor");
        let first=self.end-self.bytes.len() as u64;
        let begin=cursor.max(first);
        let bytes=self.bytes.iter().skip((begin-first) as usize).take(CHUNK).copied().collect::<Vec<_>>();
        let next=begin+bytes.len() as u64;
        Ok((bytes,next,cursor<first))
    }
}
struct Live{
    id:Uuid,owner:SessionKey,input:mpsc::Sender<Vec<u8>>,output:Mutex<Output>,
    last_seen:Mutex<Instant>,stop:AtomicBool,closed:AtomicBool,
}
#[derive(Clone,Default)]
pub(super) struct Terminals(Arc<Mutex<Option<Arc<Live>>>>);
#[derive(Deserialize)]
#[serde(tag="action",rename_all="snake_case",deny_unknown_fields)]
pub(super) enum Request{
    Open,Poll{id:Uuid,cursor:u64},Input{id:Uuid,data:String},Close{id:Uuid},
}
fn failure(status:StatusCode,message:&str)->Response{error(status,"terminal_unavailable",message)}
fn lookup(terminals:&Terminals,owner:SessionKey,id:Uuid)->Option<Arc<Live>>{
    terminals.0.lock().as_ref().filter(|s|s.owner==owner&&s.id==id).cloned()
}
pub(super) async fn handle(State(state):State<AppState>,Extension(sessions):Extension<Sessions>,
    Extension(terminals):Extension<Terminals>,headers:HeaderMap,Json(request):Json<Request>)->Response{
    if !available(&state){return failure(StatusCode::NOT_FOUND,"Browser terminal is disabled or unavailable.");}
    // Existing Linux middleware checks the origin against the configured host.
    // All terminal requests additionally require an actual browser cookie.
    let Some(owner)=sessions.browser_session(&headers).filter(|_|headers.contains_key(header::ORIGIN))else{
        return failure(StatusCode::FORBIDDEN,"A valid browser session and explicit origin are required.");
    };
    match request{
        Request::Open=>{
            let mut slot=terminals.0.lock();
            if slot.as_ref().is_some_and(|s|!s.closed.load(Ordering::Acquire)){
                return failure(StatusCode::CONFLICT,"A terminal is already active.");
            }
            let (master,child)=match spawn_pty(){Ok(pty)=>pty,Err(_)=>return failure(StatusCode::SERVICE_UNAVAILABLE,"Could not allocate a local non-root PTY.")};
            let (input,receiver)=mpsc::channel(16);
            let live=Arc::new(Live{id:Uuid::new_v4(),owner,input,output:Mutex::new(Output::default()),
                last_seen:Mutex::new(Instant::now()),stop:AtomicBool::new(false),closed:AtomicBool::new(false)});
            *slot=Some(live.clone());
            tokio::spawn(worker(master,child,receiver,live.clone(),sessions));
            Json(json!({"id":live.id})).into_response()
        }
        Request::Poll{id,cursor}=>{
            let Some(live)=lookup(&terminals,owner,id)else{return failure(StatusCode::NOT_FOUND,"Terminal is no longer available.");};
            let output=live.output.lock();
            let Ok((bytes,next,truncated))=output.read(cursor)else{return failure(StatusCode::BAD_REQUEST,"Invalid output cursor.");};
            *live.last_seen.lock()=Instant::now();
            let closed=live.closed.load(Ordering::Acquire)&&next==output.end;
            Json(json!({"data":STANDARD.encode(bytes),"cursor":next,"truncated":truncated,"closed":closed})).into_response()
        }
        Request::Input{id,data}=>{
            if data.is_empty()||data.len()>4096{return failure(StatusCode::BAD_REQUEST,"Input must contain 1–4096 bytes.");}
            let Some(live)=lookup(&terminals,owner,id).filter(|s|!s.closed.load(Ordering::Acquire)&&!s.stop.load(Ordering::Acquire))else{
                return failure(StatusCode::NOT_FOUND,"Terminal is closed.");};
            if live.input.try_send(data.into_bytes()).is_err(){return failure(StatusCode::TOO_MANY_REQUESTS,"Terminal input is busy.");}
            *live.last_seen.lock()=Instant::now();
            Json(json!({"accepted":true})).into_response()
        }
        Request::Close{id}=>{
            if let Some(live)=lookup(&terminals,owner,id){live.stop.store(true,Ordering::Release);}
            Json(json!({"closed":true})).into_response()
        }
    }
}
fn owned_fd(fd:libc::c_int)->io::Result<OwnedFd>{
    if fd<0{Err(io::Error::last_os_error())}else{Ok(unsafe{OwnedFd::from_raw_fd(fd)})}
}
fn spawn_pty()->Result<(AsyncFd<OwnedFd>,Child)>{
    let master=owned_fd(unsafe{libc::posix_openpt(libc::O_RDWR|libc::O_NOCTTY|libc::O_CLOEXEC|libc::O_NONBLOCK)})?;
    let fd=master.as_raw_fd();
    ensure!(unsafe{libc::grantpt(fd)}==0&&unsafe{libc::unlockpt(fd)}==0,"PTY allocation failed");
    let mut name=[0 as libc::c_char;128];
    ensure!(unsafe{libc::ptsname_r(fd,name.as_mut_ptr(),name.len())}==0,"PTY slave unavailable");
    let slave=owned_fd(unsafe{libc::open(name.as_ptr(),libc::O_RDWR|libc::O_NOCTTY|libc::O_CLOEXEC)})?;
    let size=libc::winsize{ws_row:30,ws_col:100,ws_xpixel:0,ws_ypixel:0};
    ensure!(unsafe{libc::ioctl(slave.as_raw_fd(),libc::TIOCSWINSZ,&size)}==0,"PTY sizing failed");
    let home=std::env::var_os("HOME").map(PathBuf::from).filter(|p|p.is_absolute()&&p.is_dir())
        .ok_or_else(||anyhow::anyhow!("workload home unavailable"))?;
    let mut command=Command::new("/bin/sh");
    command.arg("-i").env_clear().env("HOME",&home).env("PATH","/usr/local/bin:/usr/bin:/bin")
        .env("TERM","dumb").env("LANG","C.UTF-8").current_dir(&home)
        .stdin(Stdio::from(File::from(slave.try_clone()?)))
        .stdout(Stdio::from(File::from(slave.try_clone()?)))
        .stderr(Stdio::from(File::from(slave))).kill_on_drop(true);
    let parent=unsafe{libc::getpid()};
    unsafe{command.pre_exec(move||{
        if libc::setsid()<0||libc::ioctl(0,libc::TIOCSCTTY,0)<0
            ||libc::prctl(libc::PR_SET_NO_NEW_PRIVS,1,0,0,0)<0
            ||libc::prctl(libc::PR_SET_PDEATHSIG,libc::SIGHUP,0,0,0)<0
            ||libc::getppid()!=parent{return Err(io::Error::last_os_error());}
        Ok(())
    });}
    let master=AsyncFd::new(master)?;
    let child=command.spawn()?;
    Ok((master,child))
}
async fn read_pty(master:&AsyncFd<OwnedFd>,buffer:&mut[u8])->io::Result<usize>{
    loop{let mut ready=master.readable().await?;
        match ready.try_io(|fd|{
            let n=unsafe{libc::read(fd.get_ref().as_raw_fd(),buffer.as_mut_ptr().cast(),buffer.len())};
            if n<0{Err(io::Error::last_os_error())}else{Ok(n as usize)}
        }){Ok(result)=>return result,Err(_)=>continue}
    }
}
async fn write_pty(master:&AsyncFd<OwnedFd>,mut data:&[u8])->io::Result<()>{
    while !data.is_empty(){let mut ready=master.writable().await?;
        match ready.try_io(|fd|{
            let n=unsafe{libc::write(fd.get_ref().as_raw_fd(),data.as_ptr().cast(),data.len())};
            if n<0{Err(io::Error::last_os_error())}else{Ok(n as usize)}
        }){Ok(Ok(0))=>return Err(io::ErrorKind::WriteZero.into()),Ok(Ok(n))=>data=&data[n..],Ok(Err(e))=>return Err(e),Err(_)=>continue}
    }Ok(())
}
async fn worker(master:AsyncFd<OwnedFd>,mut child:Child,mut input:mpsc::Receiver<Vec<u8>>,live:Arc<Live>,sessions:Sessions){
    let started=Instant::now();let mut interval=tokio::time::interval(Duration::from_secs(1));
    let mut buffer=[0u8;8192];let mut reaped=false;
    loop{
        if live.stop.load(Ordering::Acquire)||live.last_seen.lock().elapsed()>IDLE||started.elapsed()>LIFETIME||!sessions.session_active(&live.owner){break;}
        tokio::select!{biased;
            _=interval.tick()=>{},
            _=child.wait()=>{reaped=true;break;},
            data=input.recv()=>{let Some(data)=data else{break;};
                if !matches!(tokio::time::timeout(Duration::from_secs(2),write_pty(&master,&data)).await,Ok(Ok(()))){break;}},
            result=read_pty(&master,&mut buffer)=>{match result{Ok(n) if n>0=>live.output.lock().append(&buffer[..n]),_=>break}}
        }
    }
    // Hang up the foreground group and reap the shell. Deliberately detached
    // processes are not a sandboxed workload.
    drop(master);
    if !reaped{
        if let Some(pid)=child.id(){unsafe{libc::kill(-(pid as libc::pid_t),libc::SIGKILL);}}
        let _=child.start_kill();let _=child.wait().await;
    }
    live.output.lock().append(b"\r\n[terminal disconnected]\r\n");
    live.closed.store(true,Ordering::Release);
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn bounded_output_preserves_cursor(){
        let mut out=Output::default();out.append(&vec![b'x';CAPACITY+8]);
        assert_eq!(out.bytes.len(),CAPACITY);
        let (data,cursor,truncated)=out.read(0).unwrap();
        assert!(truncated);assert_eq!(data.len(),CHUNK);assert_eq!(cursor,8+CHUNK as u64);
        assert!(out.read(out.end+1).is_err());assert!(!out.read(cursor).unwrap().2);
    }
    #[test]fn strict_request_shape(){
        assert!(serde_json::from_value::<Request>(json!({"action":"open"})).is_ok());
        assert!(serde_json::from_value::<Request>(json!({"action":"open","command":"id"})).is_err());
        assert!(serde_json::from_value::<Request>(json!({"action":"poll","cursor":0})).is_err());
    }
}
