#!/usr/bin/env node
// Offline composition/syntax checks; no browser, npm, or network required.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {Script} from 'node:vm';

execFileSync('python3', ['scripts/build-web-ui.py', '--check'], {stdio:'inherit'});
const html=readFileSync('web/index.html','utf8');
assert.equal((html.match(/<script>/g)||[]).length,1);
assert.equal((html.match(/<style>/g)||[]).length,1);
const js=html.split('<script>')[1].split('</script>')[0];
new Script(js,{filename:'composed-router-hub.js'});
for(const view of ['dashboard','services','nginx','certificates','wol','firewall','adguard','wireguard']) {
  assert.ok(html.includes('id="view-'+view+'"'),'missing '+view+' view');
  assert.ok(js.includes("load"+({nginx:'Nginx',wol:'Wol',adguard:'Adguard',wireguard:'Wireguard',firewall:'Firewall',dashboard:'Dashboard',services:'Services',certificates:'Certificates'}[view])),'missing '+view+' logic');
}
assert.match(js,/rhNavigationGroups/);
assert.match(js,/rhLoadModules/);
assert.match(js,/data-dashboard-widget/);
assert.match(js,/data-widget-state/);
assert.match(js,/rhSelectView/);
assert.ok(!html.includes('<script src='),'UI must contain no remote JS');
assert.ok(!html.includes('<link rel="stylesheet"'),'UI must contain no external CSS');
console.log('Composed UI syntax, features, and offline embedding verified');
