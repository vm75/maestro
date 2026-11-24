#!/usr/bin/env node
// Browser regression coverage of Linux shared workflows using a loopback fixture.
// Never contacts real Maestro, AdGuard, nginx, WireGuard, or firewall services.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {readFile, mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve, join, extname} from 'node:path';
import {once} from 'node:events';

const root=resolve('linux-web/dist'), lab=await mkdtemp(join(tmpdir(),'maestro-linux-ui-'));
const chrome=process.argv[2] || process.env.CHROMIUM || '/usr/bin/chromium';
const calls=[];
let allowWrites=true, reach='down', adStatus={enabled:true, protection_enabled:true, filtering_enabled:true};
let rewrites=[{domain:'old.test',answer:'192.0.2.5'}];
const modules=['nginx','certificates','ban_shield','wol','adguard','wireguard'].map(id=>({id,availability:'ready',dependencies:[]}));
const site={kind:'domain',domain:'example.test',name:'example.test',display_name:'example.test',managed:true,enabled:true,state:'live',port:8443,effective_access:{listener_profile:'legacy',access_policy:'legacy',https:[{address:'127.0.0.1',port:8443}],http:[],allowed_sources:['127.0.0.1/32'],routes:[]}};
const global={shared:{},site_defaults:{routes:[{path:'/admin',match:'exact',access_policy:'deny'}]},listener_profiles:{},access_policies:{},limit_policies:{},allowed_ips:[],free_subdomains:[]};
const tuning={ip_failures:5,subnet_failures:10,promote_after_banned_ips:2,reputation_repromote_after_offenses:1,score_retention_days:60,reputation_retention_days:365,subnet_promotion_window_days:60,subnet_ban_days:30,first_ban_days:1,second_ban_days:7,third_ban_days:30,max_ban_days:180};
let policy={enabled:true,observe_only:true,rules:[{id:'rule-1',name:'Test rule',enabled:true,log_paths:['/tmp/lab.log'],pattern:'(?P<ip>.*)',attempts:5,weight:1,ip_group:'ip',group_values:{}}],allowlist:[]};
const health={state:'running',set_capacity:8192,set_entries:1,dropped_line_count:0,command_timeout_count:0,error_count:0};
let bans=[{network:'192.0.2.2/32',reason:'test',source:'manual',offense_count:1,hit_count:2,expires_at:'2030-01-01T00:00:00Z'}];
const json=(res,value,status=200)=>{res.writeHead(status,{'Content-Type':'application/json'});res.end(JSON.stringify(value));};
const server=createServer(async(req,res)=>{
  try {
    const path=new URL(req.url,'http://127.0.0.1').pathname, method=req.method;
    if(!path.startsWith('/api/')){
      const name=path==='/'?'index.html':path.startsWith('/linux-assets/')?path.slice('/linux-assets/'.length):'';
      if(path==='/maestro.svg'){res.writeHead(200,{'Content-Type':'image/svg+xml'});res.end(await readFile('maestro.svg'));return;}
      if(!name || name.includes('..') || name.startsWith('/')){res.writeHead(404);res.end();return;}
      const mime={'.html':'text/html','.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml'};
      res.writeHead(200,{'Content-Type':mime[extname(name)] || 'application/octet-stream'});res.end(await readFile(join(root,name)));return;
    }
    const chunks=[];for await(const chunk of req) chunks.push(chunk);
    const body=chunks.length?JSON.parse(Buffer.concat(chunks).toString()):null;calls.push({path,method,body});
    if(path==='/api/status') return json(res,{hostname:'test-lab',startedAt:'2026-01-01',version:'test',allowWrites});
    if(path==='/api/tools') return json(res,{modules:[],tools:[]});
    if(path==='/api/tools/jobs_list') return json(res,[]);
    if(path==='/api/shared/modules') return json(res,{platform:'linux',modules});
    const key=path.replace('/api/shared/','');
    if(key==='nginx/status')return json(res,{running:true,config_path:'/tmp/nginx.conf',root_dir:'/tmp'});
    if(key==='nginx/sites')return json(res,[site,{...site,kind:'subfolder',name:'child',display_name:'example.test/child'}]);
    if(key==='nginx/settings')return json(res,global);
    if(key==='nginx/ingress-usage')return json(res,{});
    if(key==='nginx/logs')return json(res,[]);
    if(key==='nginx/objects/domain/example.test/example.test')return json(res,{settings:{},content:'',upstream:'http://127.0.0.1:9000',server_names:[]});
    if(key==='nginx/access-preview') {
      const access=body.settings.access_policy || 'legacy';
      return json(res,{...site.effective_access,access_policy:access,allowed_sources:access==='deny'?[]:access==='public'?null:['127.0.0.1/32'],routes:[]});
    }
    if(key==='certificates')return json(res,[]);
    if(key==='certificates/http-access')return json(res,{state:'closed',names:[],wan_control:'external'});
    if(key==='certificates/lock')return json(res,{locked:false,status:'idle'});
    if(key==='wol')return json(res,[{id:'machine-1',name:'Test machine',mac:'02:00:00:00:00:01',broadcast:'255.255.255.255',port:9,notes:'Test note'}]);
    if(key==='wol/status')return json(res,[{id:'machine-1',ip:'192.0.2.3',status:reach}]);
    if(key==='wol/machine-1/wake'){reach='up';return json(res,{machine:'Test machine',packets_sent:3,simulated:true});}
    if(key==='adguard/config')return json(res,{enabled:true,api_endpoint:'http://127.0.0.1:3000',launch_url:'http://127.0.0.1:3000',username:'',lan_ip:'127.0.0.1'});
    if(key==='adguard/status')return json(res,adStatus);
    if(key==='adguard/protection'||key==='adguard/filtering'){
      const kind=key.split('/')[1];adStatus={...adStatus,[kind+'_enabled']:body.enabled,[kind+'_remaining_seconds']:body.enabled?null:body.duration_minutes*60};return json(res,{message:'Updated'});
    }
    if(key==='adguard/rewrites'){if(method==='PUT')rewrites=body;return json(res,rewrites);}
    if(key==='wireguard/status')return json(res,{interface:'wg0',is_running:false,writable:false,peers_writable:false,reason:'Helper unavailable'});
    if(key==='firewall/capability')return json(res,{enforcement:false,reason:'Helper unavailable'});
    if(key==='firewall/rules' && method==='POST'){const rule={...body,id:'new-rule'};policy={...policy,rules:[...policy.rules,rule]};return json(res,rule);}
    if(key==='firewall'){if(method==='PUT')policy=body;return json(res,{policy,snapshot:{active_ban_count:bans.length,banned_ips:[],banned_subnets:[],rule_stats:[['Test rule',{match_count:7,ban_count:2}]]},bans,health,settings:{global_threshold:5},tuning:policy.tuning||tuning});}
    if(key.startsWith('firewall/bans/') && method==='DELETE'){bans=[];return json(res,{message:'network unbanned, history cleared, and exception added'});}
    return json(res,{error:'Unexpected fixture route: '+key},404);
  }catch(e){if(!res.headersSent)json(res,{error:String(e)},500);else res.destroy();}
});
let browser,socket;const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function waitFor(check,label){for(let i=0;i<300;i++){try{if(await check())return;}catch{}await sleep(50);}throw Error('Timed out: '+label);}
try{
  await readFile(join(root,'index.html'));
  server.listen(0,'127.0.0.1');await once(server,'listening');const base='http://127.0.0.1:'+server.address().port;
  browser=spawn(chrome,['--headless','--no-sandbox','--disable-gpu','--disable-dev-shm-usage','--disable-background-networking','--no-first-run','--remote-debugging-port=0','--user-data-dir='+join(lab,'browser'),'about:blank'],{stdio:['ignore','ignore','pipe']});
  let log='';browser.stderr.on('data',d=>log+=d.toString());
  await waitFor(()=>log.includes('DevTools listening on'),'Chromium startup');
  const debuggerUrl=log.match(/DevTools listening on (ws:\/\/[^\s]+)/)[1];
  const targets=await (await fetch(debuggerUrl.replace('ws://','http://').split('/devtools/')[0]+'/json/list')).json();
  socket=new WebSocket(targets.find(t=>t.type==='page').webSocketDebuggerUrl);await once(socket,'open');
  let seq=0;const pending=new Map(),errors=[];
  socket.addEventListener('message',event=>{const m=JSON.parse(event.data);if(m.id){const p=pending.get(m.id);pending.delete(m.id);if(m.error)p.reject(Error(m.error.message));else p.resolve(m.result);}if(m.method==='Runtime.exceptionThrown')errors.push(m.params.exceptionDetails.exception?.description||m.params.exceptionDetails.text);});
  const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));});
  const ev=async expression=>{const r=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;};
  const wait=expression=>waitFor(()=>ev(expression),expression);
  const click=async text=>{await wait('Array.from(document.querySelectorAll("button,summary")).some(b=>b.textContent.trim()==='+JSON.stringify(text)+'&&!b.disabled&&b.getClientRects().length)');await ev('Array.from(document.querySelectorAll("button,summary")).find(b=>b.textContent.trim()==='+JSON.stringify(text)+'&&!b.disabled&&b.getClientRects().length).click()');};
  const field=async(label,value)=>{await ev('(()=>{const l=[...document.querySelectorAll("label")].find(l=>l.textContent.trim().startsWith('+JSON.stringify(label)+')&&l.querySelector("input,textarea,select")&&l.getClientRects().length);const e=l.querySelector("input,textarea,select");e.value='+JSON.stringify(value)+';e.dispatchEvent(new Event("input",{bubbles:true}));e.dispatchEvent(new Event("change",{bubbles:true}));})()');};
  await call('Page.enable');await call('Runtime.enable');await call('Page.navigate',{url:base});
  await wait('document.body.innerText.includes("Module health") && document.body.innerText.includes("Renewals healthy")');
  await click('Network');await wait('document.body.innerText.includes("Test machine")');
  reach='up';await wait('document.body.innerText.includes("up 192.0.2.3")');await click('Wake');await wait('document.body.innerText.includes("Simulated 3 packets")');
  await click('AdGuard Home');await wait('document.body.innerText.includes("Protection and filtering")');await click('10 min');
  await waitFor(()=>calls.some(c=>c.path.endsWith('/adguard/protection')&&c.body.duration_minutes===10),'timed pause payload');
  await wait('document.body.innerText.includes("Paused · 10m")');await click('Resume');await waitFor(()=>calls.some(c=>c.path.endsWith('/adguard/protection')&&c.body.enabled===true),'resume payload');
  await click('Manage DNS rewrites');await field('Domain','edited.test');await click('Save rewrites');await waitFor(()=>rewrites[0].domain==='edited.test','edited rewrite');
  await click('Ban Shield');await wait('document.body.innerText.includes("7 / 2")');await click('Add rule');await field('Name','New detection');await field('Log paths','/tmp/new.log');await click('Save rule');await waitFor(()=>policy.rules.some(r=>r.name==='New detection'),'structured rule save');
  await click('Edit security tuning');await click('Load strict preset');await click('Save security tuning');await waitFor(()=>policy.tuning?.ip_failures===3,'strict preset payload');
  await ev('window.confirm=(message)=>{window.lastConfirm=message;return true}');await click('Unban + allow');await wait('window.lastConfirm.includes("allowlist")');
  await click('Web');await wait('document.body.innerText.includes("example.test/child")');await click('Edit');await wait('document.body.innerText.includes("Effective access")');await field('Access policy','deny');await wait('document.body.innerText.includes("Allowed sources: Denied")');
  await click('Path access rules and blocked paths');await field('Blocked path','/private');await click('Add blocked path');await waitFor(()=>calls.some(c=>c.path.endsWith('/nginx/access-preview')&&c.body.settings.routes?.some(r=>r.path==='/private/')&&c.body.settings.routes?.some(r=>r.path==='/admin')),'blocked path preserving inherited rules');
  await click('Setup guide');await wait('!!document.querySelector("dialog[open]")');await click('Close guide');
  await click('Certificates');await click('Add certificate');assert.equal(await ev('Array.from(document.querySelectorAll("label")).find(l=>l.textContent.trim()==="Auto renew").querySelector("input").checked'),true);
  allowWrites=false;await call('Page.navigate',{url:base+'/#web'});await call('Page.reload');await wait('document.body.innerText.includes("Read only")');await click('Inspect');await wait('document.body.innerText.includes("Inspect site")');assert.equal(await ev('!!document.querySelector("fieldset[disabled]")'),true);
  assert.deepEqual(errors,[],'browser exceptions');console.log('PASS: shared Linux UI workflows (fixture API, no host mutations)');
}finally{
  socket?.close();if(browser){browser.kill();await Promise.race([once(browser,'exit'),sleep(2000)]);}
  server.closeAllConnections();await new Promise(r=>server.close(r));await rm(lab,{recursive:true,force:true});
}
