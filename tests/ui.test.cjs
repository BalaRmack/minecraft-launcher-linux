const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { JSDOM } = require('jsdom');
const html = fs.readFileSync('ui/index.html', 'utf8');
const script = fs.readFileSync('ui/app.js', 'utf8');
const tick = () => new Promise(resolve => setTimeout(resolve, 20));
function setup() {
  const calls = [], listeners = {};
  const snapshot = { accounts: [{id:'account-id',name:'Steve'}], profiles: [{id:'profile-id',name:'<img src=x onerror=alert(1)>',version:'1.21.4',loader:'fabric',installed_id:'fabric-1.21.4'}], active_account:'account-id',active_profile:'profile-id',settings:{java:'java',memory_mb:2048} };
  const dom = new JSDOM(html, {runScripts:'outside-only',url:'https://horizon.local'});
  dom.window.confirm = () => true;
  dom.window.HTMLDialogElement.prototype.showModal = function(){this.open=true};
  dom.window.HTMLDialogElement.prototype.close = function(){this.open=false};
  dom.window.__TAURI__ = {core:{invoke:async (name,args) => {calls.push([name,args]); if(name==='snapshot')return snapshot; if(name==='versions')return [{id:'1.21.4',type:'release'},{id:'25w01a',type:'snapshot'},{id:'old',type:'old_beta'}]; if(name==='list_mods'||name==='inspect_mods'||name==='catalog_categories')return []; if(name==='catalog_search')return {hits:[{project_id:'abc',title:'Example',description:'mod',author:'Author',downloads:10}],total_hits:21}; return null;}},event:{listen:async (name, callback)=>{listeners[name]=callback;}}};
  dom.window.eval(script);
  return {dom,calls,snapshot,listeners};
}
test('renders state safely and filters Mojang versions', async () => {
 const {dom} = setup(); await tick(); const d=dom.window.document;
 assert.equal(d.querySelector('#sidebar-account').textContent,'Steve');
 assert.equal(d.querySelector('#home-profile').textContent,'<img src=x onerror=alert(1)>');
 assert.equal(d.querySelector('#profile-list img'),null);
 d.querySelector('#new-profile').click(); d.querySelector('#wizard-version').click(); await tick();
 assert.equal(d.querySelector('select'),null); assert.deepEqual([...d.querySelectorAll('.version-row')].map(x=>x.dataset.id),['1.21.4']);
 d.querySelector('[data-version-filter=snapshot]').click(); assert.deepEqual([...d.querySelectorAll('.version-row')].map(x=>x.dataset.id),['25w01a']);
 d.querySelector('[data-version-filter=installed]').click(); assert.deepEqual([...d.querySelectorAll('.version-row')].map(x=>x.dataset.id),['1.21.4']);
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

test('game process failures are surfaced as errors', async () => {
 const {dom,listeners}=setup(); await tick(); listeners['game-exit']({payload:{success:false,status:'exit status: 1'}});
 assert.match(dom.window.document.querySelector('#status').textContent,/Ошибка игры/); dom.window.close();
});

test('wizard creates a profile using the custom version picker', async () => {
 const {dom,calls}=setup(); await tick(); const d=dom.window.document; d.querySelector('#new-profile').click(); d.querySelector('#wizard-version').click(); await tick(); d.querySelector('[data-action=choose-version]').click(); d.querySelector('[data-loader=forge]').click(); const form=d.querySelector('#profile-form');form.elements.name.value='Test build';form.dispatchEvent(new dom.window.SubmitEvent('submit',{bubbles:true,cancelable:true,submitter:form.querySelector('[type=submit]')}));await tick(); const call=calls.find(x=>x[0]==='create_profile');assert.equal(call[1].version,'1.21.4');assert.equal(call[1].loader,'forge');dom.window.close();
});
test('catalog filters by profile, paginates and opens installed tab', async () => {
 const {dom,calls}=setup(); await tick(); const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();let call=calls.filter(x=>x[0]==='catalog_search').at(-1);assert.equal(call[1].params.version,'1.21.4');assert.equal(call[1].params.loader,'fabric');assert.equal(d.querySelectorAll('.mod-card').length,1);d.querySelector('#catalog-next').click();await tick();call=calls.filter(x=>x[0]==='catalog_search').at(-1);assert.equal(call[1].params.offset,20);d.querySelector('[data-mod-tab=installed]').click();await tick();assert.equal(calls.some(x=>x[0]==='inspect_mods'),true);dom.window.close();
});
