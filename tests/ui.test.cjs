const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { JSDOM } = require('jsdom');
const html = fs.readFileSync('ui/index.html', 'utf8');
const script = fs.readFileSync('ui/app.js', 'utf8');
const tick = () => new Promise(resolve => setTimeout(resolve, 20));
function setup() {
  const calls = [];
  const snapshot = { accounts: [{id:'account-id',name:'Steve'}], profiles: [{id:'profile-id',name:'<img src=x onerror=alert(1)>',version:'1.21.4',loader:'fabric',installed_id:'fabric-1.21.4'}], active_account:'account-id',active_profile:'profile-id',settings:{java:'java',memory_mb:2048} };
  const dom = new JSDOM(html, {runScripts:'outside-only',url:'https://horizon.local'});
  dom.window.confirm = () => true;
  dom.window.__TAURI__ = {core:{invoke:async (name,args) => {calls.push([name,args]); if(name==='snapshot')return snapshot; if(name==='versions')return [{id:'1.21.4',type:'release'},{id:'25w01a',type:'snapshot'},{id:'old',type:'old_beta'}]; if(name==='list_mods')return []; return null;}},event:{listen:async()=>{}}};
  dom.window.eval(script);
  return {dom,calls,snapshot};
}
test('renders state safely and filters Mojang versions', async () => {
 const {dom} = setup(); await tick(); const d=dom.window.document;
 assert.equal(d.querySelector('#sidebar-account').textContent,'Steve');
 assert.equal(d.querySelector('#home-profile').textContent,'<img src=x onerror=alert(1)>');
 assert.equal(d.querySelector('#profile-list img'),null);
 assert.equal(d.querySelectorAll('#version-select option').length,3);
 dom.window.close();
});
test('creates offline account through native command', async () => {
 const {dom,calls}=setup(); await tick(); const form=dom.window.document.querySelector('#account-form'); form.elements.name.value='Alex'; form.dispatchEvent(new dom.window.SubmitEvent('submit',{bubbles:true,cancelable:true,submitter:form.querySelector('button[type=submit]')})); await tick();
 const call=calls.find(x=>x[0]==='save_account'); assert.equal(call[1].name,'Alex'); assert.equal(call[1].id,null); dom.window.close();
});
test('launch passes the selected profile, not a shell command', async () => {
 const {dom,calls}=setup(); await tick(); dom.window.document.querySelector('#play').click(); await tick(); const call=calls.find(x=>x[0]==='launch_profile'); assert.equal(call[1].id,'profile-id'); dom.window.close();
});
test('deletion requires confirmation', async () => {
 const {dom,calls}=setup(); await tick(); dom.window.confirm=()=>false; dom.window.document.querySelector('[data-action="delete-profile"]').click(); await tick(); assert.equal(calls.some(x=>x[0]==='delete_profile'),false); dom.window.close();
});
test('profile settings send overrides through native IPC', async () => {
 const {dom,calls}=setup(); await tick(); const d=dom.window.document;
 const dialog=d.querySelector('#profile-dialog'); dialog.showModal=()=>{}; dialog.close=()=>{};
 d.querySelector('[data-action="settings-profile"]').click(); await tick();
 const form=d.querySelector('#profile-settings-form'); form.elements.java.value='/usr/bin/java'; form.elements.memory_mb.value='4096';
 form.dispatchEvent(new dom.window.SubmitEvent('submit',{bubbles:true,cancelable:true,submitter:form.querySelector('button[type=submit]')})); await tick();
 const call=calls.find(x=>x[0]==='save_profile_settings'); assert.equal(call[1].id,'profile-id'); assert.equal(call[1].java,'/usr/bin/java'); assert.equal(call[1].memoryMb,4096); dom.window.close();
});
