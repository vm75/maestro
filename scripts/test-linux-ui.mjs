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
let adEnabled=true,adUnavailable=false;
let rewrites=[{domain:'old.test',answer:'192.0.2.5'},{domain:'another.test',answer:'192.0.2.7'}];
let certs=[], certLock={locked:false,status:'idle'}, httpAccess={state:'closed',names:[],wan_control:'external'};
let wgAuthorized=false,wgRunning=false,wgPeers=[],wgRevision='1';
const modules=['nginx','certificates','ban_shield','wol','adguard','wireguard'].map(id=>({id,availability:'ready',dependencies:[]}));
const site={kind:'domain',domain:'example.test',name:'example.test',display_name:'example.test',managed:true,enabled:true,state:'live',port:8443,effective_access:{listener_profile:'legacy',access_policy:'legacy',https:[{address:'127.0.0.1',port:8443}],http:[],allowed_sources:['127.0.0.1/32'],routes:[]}};
let global={shared:{},site_defaults:{disable_root:false,routes:[{path:'/admin',match:'exact',access_policy:'deny'}]},listener_profiles:{lab_listener:{https:[{address:'127.0.0.1',port:8444}],http:[],http_redirect:false}},access_policies:{lab_access:{allow:['192.0.2.0/24']}},limit_policies:{},allowed_ips:[],free_subdomains:[]};
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
    if(key==='nginx/settings'){if(method==='PUT')global=body;return json(res,global);}
    if(key==='nginx/ingress-usage')return json(res,{});
    if(key==='nginx/logs')return json(res,[{path:'lab.log',size:2048}]);
    if(key==='nginx/logs/lab.log')return json(res,{content:Array.from({length:100},(_,i)=>'Test log line '+i).join('\n')});
    if(key==='nginx/objects/domain/example.test/example.test')return json(res,{settings:{},content:'',upstream:'http://127.0.0.1:9000',server_names:[]});
    if(key==='nginx/access-preview') {
      const access=body.settings.access_policy || 'legacy';
      if(access==='public')await sleep(700);
      return json(res,{...site.effective_access,access_policy:access,allowed_sources:access==='deny'?[]:access==='public'?null:['127.0.0.1/32'],routes:[]});
    }
    if(key==='certificates'){
      if(method==='POST'){certs.push({...body,id:'cert-1',exists:false,renewal_due:true});return json(res,{message:'Certificate definition created'});}
      return json(res,certs);
    }
    if(key==='certificates/http-access'){if(method==='DELETE')httpAccess={state:'closed',names:[],wan_control:'external'};return json(res,httpAccess);}
    if(key==='certificates/lock')return json(res,certLock);
    if(key==='certificates/lock/clear'){certLock={locked:false,status:'idle'};return json(res,certLock);}
    if(key==='certificates/cert-1'){
      if(method==='PUT'){certs[0]={...certs[0],...body};return json(res,{message:'Certificate definition updated'});}
      if(method==='DELETE'){certs=[];return json(res,{message:'Certificate definition removed'});}
    }
    if(key==='certificates/cert-1/issue'||key==='certificates/cert-1/renew'){
      const action=key.split('/').at(-1);
      if(certLock.locked||certLock.status==='running')return json(res,{error:'Certificate operation locked'},409);
      certLock={locked:true,status:'running',operation:{certificate:certs[0].name,action,status:'running'}};
      httpAccess={state:'active',names:certs[0].domains,wan_control:'external',expires_at:'2030-01-01T00:00:00Z'};
      await sleep(1000);
      certs[0]={...certs[0],exists:true,renewal_due:false,days_remaining:90,expires_at:'2030-01-01T00:00:00Z'};
      certLock={locked:false,status:'completed',operation:{certificate:certs[0].name,action,status:'completed'}};
      httpAccess={state:'closed',names:[],wan_control:'external'};
      return json(res,{message:action==='issue'?'Certificate issued':'Certificate renewed'});
    }
    if(key==='wol')return json(res,[{id:'machine-1',name:'Test machine',mac:'02:00:00:00:00:01',broadcast:'255.255.255.255',port:9,notes:'Test note'}]);
    if(key==='wol/status')return json(res,[{id:'machine-1',ip:'192.0.2.3',status:reach}]);
    if(key==='wol/machine-1/wake'){reach='up';return json(res,{machine:'Test machine',packets_sent:3,simulated:true});}
    if(key==='adguard/config')return json(res,{enabled:adEnabled,api_endpoint:'http://127.0.0.1:3000',launch_url:'http://127.0.0.1:3000',username:'',lan_ip:'127.0.0.1'});
    if(key==='adguard/status')return adUnavailable?json(res,{error:'Fixture API offline'},503):json(res,adStatus);
    if(key==='adguard/protection'||key==='adguard/filtering'){
      const kind=key.split('/')[1];adStatus={...adStatus,[kind+'_enabled']:body.enabled,[kind+'_remaining_seconds']:body.enabled?null:body.duration_minutes*60};return json(res,{message:'Updated'});
    }
    if(key==='adguard/rewrites'){if(method==='PUT')rewrites=body;return json(res,rewrites);}
    if(key==='wireguard/status')return json(res,{interface:'wg0',is_running:wgRunning,writable:wgAuthorized,peers_writable:wgAuthorized,reason:wgAuthorized?'Authorized system service':'Helper unavailable'});
    if(key==='wireguard/peers'){
      if(!wgAuthorized)return json(res,{error:'Helper unavailable'},400);
      if(method==='PUT'){
        if(wgRunning||body.revision!==wgRevision)return json(res,{error:'Configuration changed or running'},409);
        wgPeers=body.peers;wgRevision=String(Number(wgRevision)+1);
      }
      return json(res,{revision:wgRevision,peers:wgPeers,address:'10.0.0.1/24',listen_port:51820,is_running:wgRunning});
    }
    if(key==='wireguard/toggle'){
      if(!wgAuthorized)return json(res,{error:'Helper unavailable'},400);
      wgRunning=body.enable;return json(res,{message:wgRunning?'Started':'Stopped'});
    }
    if(key==='firewall/capability')return json(res,{enforcement:false,reason:'Helper unavailable'});
    if(key==='firewall/rules' && method==='POST'){const rule={...body,id:'new-rule'};policy={...policy,rules:[...policy.rules,rule]};return json(res,rule);}
    if(key==='firewall'){if(method==='PUT')policy=body;return json(res,{policy,snapshot:{active_ban_count:bans.length,banned_ips:[],banned_subnets:[],rule_stats:[['Test rule',{match_count:7,ban_count:2}]]},bans,health,settings:{global_threshold:5},tuning:policy.tuning||tuning});}
    if(key==='firewall/bans' && method==='GET')return json(res,bans);
    if(key.startsWith('firewall/bans/') && method==='DELETE'){bans=[];return json(res,{message:'network unbanned, history cleared, and exception added'});}
    return json(res,{error:'Unexpected fixture route: '+key},404);
  }catch(e){if(!res.headersSent)json(res,{error:String(e)},500);else res.destroy();}
});
let browser,socket,diagnostic;const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function waitFor(check,label,attempts=300){let lastError;for(let i=0;i<attempts;i++){try{if(await check())return;}catch(e){lastError=e;}await sleep(50);}throw Error('Timed out: '+label+(lastError?' ('+lastError.message+')':''));}
try{
  await readFile(join(root,'index.html'));
  server.listen(0,'127.0.0.1');await once(server,'listening');const base='http://127.0.0.1:'+server.address().port;
  browser=spawn(chrome,['--headless=new','--no-sandbox','--disable-gpu','--disable-dev-shm-usage','--disable-background-networking','--disable-component-update','--no-first-run','--no-default-browser-check','--remote-debugging-address=127.0.0.1','--remote-debugging-port=0','--user-data-dir='+join(lab,'browser'),'about:blank'],{stdio:['ignore','pipe','pipe']});
  let log='',launchError,port;browser.stderr.on('data',d=>log+=d.toString());browser.stdout.on('data',d=>log+=d.toString());browser.on('error',e=>{launchError=e;});
  try { await waitFor(async()=>{if(launchError||browser.exitCode!==null)return true;const value=await readFile(join(lab,'browser','DevToolsActivePort'),'utf8');port=Number(value.split('\n')[0]);return port>0;},'Chromium startup',600); } catch(e){throw Error(e.message+'\n'+log);}
  if(launchError||!port)throw Error('Chromium startup failed: '+(launchError?.message||browser.exitCode)+'\n'+log);
  let targets;await waitFor(async()=>{targets=await (await fetch('http://127.0.0.1:'+port+'/json/list')).json();return targets.some(t=>t.type==='page');},'Chromium page target');
  socket=new WebSocket(targets.find(t=>t.type==='page').webSocketDebuggerUrl);await once(socket,'open');
  let seq=0;const pending=new Map(),errors=[];
  socket.addEventListener('message',event=>{const m=JSON.parse(event.data);if(m.id){const p=pending.get(m.id);pending.delete(m.id);if(m.error)p.reject(Error(m.error.message));else p.resolve(m.result);}if(m.method==='Runtime.exceptionThrown')errors.push(m.params.exceptionDetails.exception?.description||m.params.exceptionDetails.text);});
  const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));});
  const ev=async expression=>{const r=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;};
  diagnostic=async()=>({body:await ev('document.body?.innerText'),errors,calls:calls.slice(-12)});
  const wait=expression=>waitFor(()=>ev(expression),expression);
  const click=async text=>{await wait('Array.from(document.querySelectorAll("button,summary")).some(b=>b.textContent.trim()==='+JSON.stringify(text)+'&&!b.matches(\":disabled\")&&b.getClientRects().length)');await ev('Array.from(document.querySelectorAll("button,summary")).find(b=>b.textContent.trim()==='+JSON.stringify(text)+'&&!b.matches(\":disabled\")&&b.getClientRects().length).click()');};
  const field=async(label,value)=>{await ev('(()=>{const l=[...document.querySelectorAll("label")].find(l=>l.textContent.trim().startsWith('+JSON.stringify(label)+')&&l.querySelector("input,textarea,select")&&l.getClientRects().length);const e=l.querySelector("input,textarea,select");e.value='+JSON.stringify(value)+';e.dispatchEvent(new Event("input",{bubbles:true}));e.dispatchEvent(new Event("change",{bubbles:true}));})()');};
  await call('Page.enable');await call('Runtime.enable');await call('Page.navigate',{url:base});
  await wait('document.body.innerText.includes("Module health") && document.body.innerText.includes("Renewals healthy")');
  await click('Network');await wait('document.body.innerText.includes("Test machine")');await click('Notes');await wait('document.querySelector("dialog[open]").innerText.includes("Test note")');await click('Close notes');
  reach='up';await wait('document.body.innerText.includes("up 192.0.2.3")');await click('Wake');await wait('document.body.innerText.includes("Simulated 3 packets")');
  await click('AdGuard Home');await wait('document.body.innerText.includes("Protection and filtering")');await click('10 min');
  await waitFor(()=>calls.some(c=>c.path.endsWith('/adguard/protection')&&c.body.duration_minutes===10),'timed pause payload');
  await wait('document.body.innerText.includes("Paused · 10m")');await click('Resume');await waitFor(()=>calls.some(c=>c.path.endsWith('/adguard/protection')&&c.body.enabled===true),'resume payload');
  await click('Answer ↕');assert.equal(await ev('document.querySelector("tbody tr td").textContent'),'old.test');await click('Answer ↑');assert.equal(await ev('document.querySelector("tbody tr td").textContent'),'another.test');
  rewrites.push({domain:'external.test',answer:'192.0.2.8'});const rewriteReads=calls.filter(c=>c.path.endsWith('/adguard/rewrites')&&c.method==='GET').length;
  await click('Manage DNS rewrites');await wait('document.body.innerText.includes("Save rewrites")');assert.ok(calls.filter(c=>c.path.endsWith('/adguard/rewrites')&&c.method==='GET').length>rewriteReads);assert.ok(await ev('[...document.querySelectorAll("input")].some(e=>e.value==="external.test")'),'fresh external rewrite');
  await click('Sort answer ↕');assert.equal(await ev('[...document.querySelectorAll("label")].find(l=>l.textContent.trim()==="Answer").querySelector("input").value'),'192.0.2.5');await field('Domain','edited.test');await click('Save rewrites');await waitFor(()=>rewrites[0].domain==='edited.test'&&rewrites.some(r=>r.domain==='external.test'),'edited fresh rewrite');
  adUnavailable=true;await click('Refresh');await wait('document.body.innerText.includes("Fixture API offline")');assert.ok(await ev('[...document.querySelectorAll("button")].some(b=>b.textContent.trim()==="Launch Web UI"&&!b.disabled)'));adUnavailable=false;
  adEnabled=false;await click('Refresh');await wait('!document.body.innerText.includes("Protection and filtering")');assert.ok(await ev('[...document.querySelectorAll("button")].some(b=>b.textContent.trim()==="Launch Web UI"&&!b.disabled)'));adEnabled=true;
  await click('WireGuard');await wait('document.body.innerText.includes("Helper unavailable")');assert.ok(await ev('[...document.querySelectorAll("button")].find(b=>b.textContent.trim()==="Start").disabled'));assert.ok(!calls.some(c=>c.path.endsWith('/wireguard/peers')),'do not read private configuration without grant');
  wgAuthorized=true;await click('Refresh');await wait('document.body.innerText.includes("WireGuard peers")');await field('Public key','AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=');await field('Allowed IP networks','10.0.0.2/32');await click('Save peer');await waitFor(()=>wgPeers.length===1,'authorized peer save');await click('Manage peers');await click('Add draft peer');
  await ev('(()=>{const labels=[...document.querySelectorAll("label")];for(const [name,value] of [["Batch public key","BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB="],["Batch allowed IP networks","10.0.0.3/32"]]){const e=labels.filter(l=>l.textContent.trim()===name).at(-1).querySelector("input");e.value=value;e.dispatchEvent(new Event("input",{bubbles:true}));}})()');
  await field('Batch allowed IP networks','10.0.0.4/32');const peerWrites=calls.filter(c=>c.path.endsWith('/wireguard/peers')&&c.method==='PUT').length;
  await click('Save peers');await waitFor(()=>wgPeers.length===2&&wgPeers[0].allowed_ips==='10.0.0.4/32','batch save');assert.equal(calls.filter(c=>c.path.endsWith('/wireguard/peers')&&c.method==='PUT').length,peerWrites+1);
  await click('Manage peers');await click('Add draft peer');await click('Cancel peer edits');assert.equal(wgPeers.length,2);
  await click('Manage peers');wgRevision=String(Number(wgRevision)+1);await click('Refresh');await wait('document.body.innerText.includes("Edit peer list")');await click('Save peers');await wait('document.body.innerText.includes("Peer configuration changed")');await click('Cancel peer edits');await click('Start');await waitFor(()=>wgRunning,'authorized start');await wait('[...document.querySelectorAll("button")].find(b=>b.textContent.trim()==="Save peer").closest("fieldset").disabled');await ev('window.confirm=()=>true');await click('Stop');await waitFor(()=>!wgRunning,'authorized stop');
  await click('Ban Shield');await wait('document.body.innerText.includes("7 / 2")');await click('Add rule');await field('Name','New detection');await field('Log paths','/tmp/new.log');await click('Save rule');await waitFor(()=>policy.rules.some(r=>r.name==='New detection'),'structured rule save');
  await click('Edit security tuning');await wait('document.body.innerText.includes("IP failure threshold")&&document.body.innerText.includes("carrier networks")');await click('Load strict preset');await click('Save security tuning');await waitFor(()=>policy.tuning?.ip_failures===3,'strict preset payload');
  await ev('window.confirm=(message)=>{window.lastConfirm=message;return true}');assert.equal(calls.some(c=>c.path==='/api/shared/firewall/bans'&&c.method==='GET'),false,'ban details must load on demand');await click('Show active bans');await click('Expires ↕');await click('Unban + allow');await wait('window.lastConfirm.includes("allowlist")');
  await click('Web');await wait('document.body.innerText.includes("example.test/child")');await click('Edit');await wait('document.body.innerText.includes("Effective access")');await field('Access policy','public');await waitFor(()=>calls.some(c=>c.path.endsWith('/nginx/access-preview')&&c.body?.settings?.access_policy==='public'),'slow public preview requested');await field('Access policy','deny');await wait('document.body.innerText.includes("Allowed sources: Denied")');await sleep(800);assert.ok(await ev('document.body.innerText.includes("Allowed sources: Denied")'),'stale preview must not overwrite current policy');
  await field('Upstream','http://127.0.0.1:8123');await click('Global settings');await field('Listener name','renamed_listener');await field('Access rule name','renamed_access');
  await ev('(()=>{const label=[...document.querySelectorAll("label")].find(l=>l.textContent.includes("Configure HTTP access lifecycle"));label.querySelector("input").click()})()');await wait('document.body.innerText.includes("DNS-01 never opens a window")&&document.body.innerText.includes("requires explicit external WAN management")');await field('HTTP mode','off');await wait('document.body.innerText.includes("HTTP-01 is disabled")');await field('HTTP mode','always');await wait('[...document.querySelectorAll("label")].find(l=>l.textContent.includes("WAN access managed externally")).querySelector("input").disabled');await field('HTTP mode','renewal_only');
  await click('Advanced global settings');await field('Run nginx in background','false');await wait('document.body.innerText.includes("foreground Nginx prevents")');await field('Run nginx in background','true');
  await click('Save global settings');await waitFor(()=>global.listener_profiles.renamed_listener&&global.access_policies.renamed_access&&!global.listener_profiles.lab_listener&&!global.access_policies.lab_access,'renamed profiles saved');await wait('!document.body.innerText.includes("Save global settings")');assert.equal(await ev('[...document.querySelectorAll("label")].find(l=>l.textContent.trim()==="Upstream").querySelector("input").value'),'http://127.0.0.1:8123');
  await field('Return 404 at domain root','true');await wait('document.body.innerText.includes("Domain container:")');assert.equal(await ev('[...document.querySelectorAll("label")].some(l=>l.textContent.trim()==="Upstream"&&l.getClientRects().length>0)'),false);await field('Return 404 at domain root','false');await field('Transport','sse');assert.equal(await ev('[...document.querySelectorAll("label")].some(l=>l.textContent.trim().startsWith("Connect timeout"))'),false);
  await click('Path access rules and blocked paths');await field('Blocked path','/private');await click('Add blocked path');await waitFor(()=>calls.some(c=>c.path.endsWith('/nginx/access-preview')&&c.body.settings.routes?.some(r=>r.path==='/private/')&&c.body.settings.routes?.some(r=>r.path==='/admin')),'blocked path preserving inherited rules');
  await click('Read log');await wait('document.body.innerText.includes("Test log line 99")');assert.ok(await ev('[...document.querySelectorAll("pre")].some(e=>e.textContent.includes("Test log line 99")&&e.scrollTop+e.clientHeight>=e.scrollHeight-2)'),'log scrolled to end');await click('Setup guide');await wait('!!document.querySelector("dialog[open]")');await click('Close guide');
  await call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:true});await click('Open guide');await wait('!!document.querySelector("dialog[open]")');assert.ok(await ev('document.querySelector("dialog[open]").getBoundingClientRect().width<=window.innerWidth'),'guide fits mobile viewport');await click('Close guide');await call('Emulation.clearDeviceMetricsOverride');
  await click('Certificates');await click('Add certificate');assert.equal(await ev('Array.from(document.querySelectorAll("label")).find(l=>l.textContent.trim()==="Auto renew").querySelector("input").checked'),true);
  await field('Name','Lab certificate');await field('Domains','example.test');await click('Save certificate');await waitFor(()=>certs.length===1,'certificate definition saved');await wait('document.body.innerText.includes("Lab certificate")');
  certLock={locked:true,status:'locked',path:'/tmp/lab.lock'};await click('Refresh');await wait('document.body.innerText.includes("/tmp/lab.lock")');assert.ok(await ev('[...document.querySelectorAll("button")].find(b=>b.textContent.trim()==="Issue").disabled'));await click('Clear stale lock');await waitFor(()=>!certLock.locked,'stale lock cleared');
  await click('Issue');await wait('document.body.innerText.includes("Certificate issuance in progress for Lab certificate")');await wait('document.body.innerText.includes("Certificate validation in progress")');await waitFor(()=>certs[0].exists,'fixture issuance');await wait('document.body.innerText.includes("90 days remaining")');await click('Renew');await wait('document.body.innerText.includes("Certificate renew: Lab certificate")');
  httpAccess={state:'cleanup_failed',names:['example.test'],wan_control:'external',expires_at:'2030-01-01T00:00:00Z',error:'Fixture cleanup failed'};await click('Refresh');await wait('document.body.innerText.includes("Cleanup failed / exposure unknown")&&document.body.innerText.includes("Fixture cleanup failed")');await click('Close validation window');await waitFor(()=>httpAccess.state==='closed','HTTP validation closed');
  await click('Edit');await field('Name','Renamed certificate');await click('Save certificate');await waitFor(()=>certs[0].name==='Renamed certificate','certificate edited');await click('Delete');await waitFor(()=>certs.length===0,'certificate deleted');
  allowWrites=false;await call('Page.navigate',{url:base+'/#web'});await call('Page.reload');await wait('document.body.innerText.includes("Read only")');await click('Inspect');await wait('document.body.innerText.includes("Inspect site")');assert.equal(await ev('!!document.querySelector("fieldset[disabled]")'),true);
  assert.deepEqual(errors,[],'browser exceptions');console.log('PASS: shared Linux UI workflows (fixture API, no host mutations)');
}catch(e){console.error(e);if(diagnostic){try{console.error(JSON.stringify(await diagnostic(),null,2))}catch{}}throw e}finally{
  socket?.close();if(browser && browser.exitCode===null && browser.signalCode===null){browser.kill();await Promise.race([once(browser,'exit'),sleep(3000)]);}
  server.closeAllConnections();await new Promise(r=>server.close(r));try{await rm(lab,{recursive:true,force:true,maxRetries:10,retryDelay:200})}catch(e){console.warn('Browser cleanup:',e.message)}
}
