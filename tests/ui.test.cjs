const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const { JSDOM } = require('jsdom');
const html = fs.readFileSync('ui/index.html', 'utf8');
const script = fs.readFileSync('ui/app.js', 'utf8');
const tick = () => new Promise(resolve => setTimeout(resolve, 20));
function setup(overrides = {}) {
  const calls = [], listeners = {};
  const snapshot = { accounts: [{id:'account-id',name:'Steve'}], profiles: [{id:'profile-id',name:'<img src=x onerror=alert(1)>',version:'1.21.4',loader:'fabric',installed_id:'fabric-1.21.4'}], active_account:'account-id',active_profile:'profile-id',settings:{java:'java',memory_mb:2048} };
  const dom = new JSDOM(html, {runScripts:'outside-only',url:'https://horizon.local'});
  dom.window.confirm = () => true;
  dom.window.HTMLDialogElement.prototype.showModal = function(){this.open=true};
  dom.window.HTMLDialogElement.prototype.close = function(){this.open=false};
  dom.window.__TAURI__ = {core:{invoke:async (name,args) => {calls.push([name,args]); if (overrides[name]) return overrides[name](args); if(name==='snapshot')return snapshot; if(name==='profile_status')return {state:'ready',path:'/game'}; if(name==='versions')return [{id:'1.21.4',type:'release'},{id:'25w01a',type:'snapshot'},{id:'old',type:'old_beta'}]; if(name==='list_mods'||name==='inspect_mods'||name==='catalog_categories')return []; if(name==='catalog_search')return {hits:[{project_id:'abc',title:'Example',description:'mod',author:'Author',downloads:10}],total_hits:21}; return null;}},event:{listen:async (name, callback)=>{listeners[name]=callback;}}};
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
 d.querySelector('[data-action="open-instance"]').click(); await tick(); d.querySelector('[data-instance-tab=settings]').click();
 const form=d.querySelector('#instance-settings-form'); form.elements.java.value='/usr/bin/java'; form.elements.memory_mb.value='4096';
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

test('profile page hosts mods and settings for the selected build', async () => {
 const {dom,calls}=setup();await tick();const d=dom.window.document;d.querySelector('[data-action=open-instance]').click();await tick();assert.ok(d.querySelector('#instance.active'));assert.equal(d.querySelectorAll('.build-card .card-actions > .build-play').length,1);assert.match(d.querySelector('.build-card .build-play').textContent,/Играть/);d.querySelector('[data-instance-tab=mods]').click();await tick();assert.ok(d.querySelector('#instance-mods-slot #mods-workspace'));assert.equal(calls.find(x=>x[0]==='catalog_search')[1].params.loader,'fabric');d.querySelector('[data-instance-tab=settings]').click();assert.equal(d.querySelector('#instance-settings').classList.contains('hidden'),false);dom.window.close();
});
test('installation shows real operation state and requests cancellation', async () => {
 let resolveInstall;let current='not_installed';const {dom,calls}=setup({profile_status:async()=>({state:current,path:'/game'}),install_profile:()=>new Promise(resolve=>{resolveInstall=resolve;})});await tick();const d=dom.window.document;assert.match(d.querySelector('.build-card .build-play').textContent,/Установить/);d.querySelector('.build-card [data-action=install]').click();await tick();assert.equal(d.querySelector('.build-card [data-action=play-profile]'),null);d.querySelector('.build-card [data-action=cancel-install]').click();await tick();assert.ok(calls.some(x=>x[0]==='cancel_install'&&x[1].id==='profile-id'));current='ready';resolveInstall();await tick();await tick();assert.match(d.querySelector('.build-card .build-play').textContent,/Играть/);dom.window.close();
});
test('broken files offer repair rather than play', async () => {
 const {dom,calls}=setup({profile_status:async()=>({state:'broken',reason:'Повреждён клиент',path:'/game'})});await tick();const d=dom.window.document;assert.match(d.querySelector('.build-card .build-play').textContent,/Восстановить/);assert.equal(d.querySelector('.build-card [data-action=play-profile]'),null);d.querySelector('[data-action=open-instance]').click();await tick();assert.match(d.querySelector('#instance-overview').textContent,/Повреждён клиент/);dom.window.close();
});

test('account selection and confirmed profile deletion call native commands', async () => {
 const {dom,calls,snapshot}=setup();snapshot.accounts.push({id:'second',name:'Alex'});await tick();const d=dom.window.document;d.querySelector('[data-action=select-account][data-id=second]').click();await tick();assert.ok(calls.some(([name,args])=>name==='select_item'&&args.kind==='account'&&args.id==='second'));d.querySelector('[data-action=delete-profile]').click();await tick();assert.ok(calls.some(([name,args])=>name==='delete_profile'&&args.id==='profile-id'));dom.window.close();
});
test('installation progress distinguishes unknown total from file percentage and cancellation asks', async () => {
 let resolveInstall; const {dom,calls,listeners}=setup({profile_status:async()=>({state:'not_installed',path:'/game'}),install_profile:()=>new Promise(resolve=>resolveInstall=resolve)}); await tick();const d=dom.window.document;d.querySelector('.build-card [data-action=install]').click();await tick();
 listeners['profile-progress']({payload:{id:'profile-id',message:'Установка Minecraft',phase_done:3,phase_total:10,downloaded:2,cached:1,remaining_phase:7,speed_mbps:1.25,filename:'library long-file.jar',path:'/full/path/long-file.jar',file_received:50,file_total:200}});
 const card=d.querySelector('.build-card .install-state');assert.match(card.textContent,/Общий прогресс уточняется/);assert.match(card.textContent,/25% файла/);assert.match(card.textContent,/3 \/ 10 в этапе/);assert.match(card.textContent,/1.25 МБ\/с/);assert.equal(card.querySelector('.install-file').title,'/full/path/long-file.jar');assert.equal(card.querySelector('[role=progressbar]').getAttribute('aria-valuenow'),'25');dom.window.confirm=()=>false;card.querySelector('[data-action=cancel-install]').click();await tick();assert.equal(calls.some(x=>x[0]==='cancel_install'),false);resolveInstall();await tick();dom.window.close();
});
test('switch loader checks mods, requires confirmation and installs a separate profile', async () => {
 const {dom,calls,snapshot}=setup({available_loaders:async()=>['1.20.1-47.4.0','1.20.1-47.3.0'],preview_loader_mods:async()=>[{name:'local.jar',enabled:true,version:null,warnings:[]}],copy_with_loader:async()=>{snapshot.profiles.push({id:'copy',name:'copy',version:'1.21.4',loader:'forge'});snapshot.active_profile='copy';return 'copy';}});await tick();const d=dom.window.document;d.querySelector('[data-action=open-instance]').click();await tick();d.querySelector('[data-instance-tab=settings]').click();d.querySelector('#change-loader').click();d.querySelector('[data-switch-loader=forge]').click();await tick();assert.match(d.querySelector('#loader-mod-warnings').textContent,/совместимость 1 локальных/);d.querySelector('[data-id="1.20.1-47.3.0"]').click();dom.window.confirm=()=>false;d.querySelector('#confirm-loader').click();await tick();assert.equal(calls.some(c=>c[0]==='copy_with_loader'),false);dom.window.confirm=()=>true;d.querySelector('#confirm-loader').click();await tick();await tick();const copy=calls.find(c=>c[0]==='copy_with_loader');assert.equal(copy[1].id,'profile-id');assert.equal(copy[1].loader,'forge');assert.equal(copy[1].loaderVersion,'1.20.1-47.3.0');assert.ok(calls.some(c=>c[0]==='install_profile'&&c[1].id==='copy'));dom.window.close();
});

test('catalog follows per-profile files across install, restart, removal and update', async () => {
 const files={a:[],b:[]}, installed={name:'xaero.jar',enabled:true,version:{project_id:'xaero',id:'old',version_number:'1.0'},update:null};
 let setupApp;
 const opts={catalog_search:async()=>({hits:[{project_id:'xaero',title:"Xaero's Minimap",downloads:10}],total_hits:1}),mod_status:async({id})=>({files:files[id].map(x=>({...x})),lookup_complete:true,unknown:0}),install_mod:async({id,update})=>{files[id]=[{...installed,version:{...installed.version,id:update?'new':'old'}}];return 1;},list_mods:async({id})=>files[id].map(x=>({name:x.name,enabled:x.enabled})),inspect_mods:async({id})=>files[id].map(x=>({...x,project:{id:'xaero',title:"Xaero's Minimap"},warnings:[]})),change_mod:async({id})=>{files[id]=[];},select_item:async({id})=>{setupApp.snapshot.active_profile=id;}};
 function init(){setupApp=setup(opts);setupApp.snapshot.active_profile='a';setupApp.snapshot.profiles=[{id:'a',name:'First',version:'1.20.1',loader:'fabric'},{id:'b',name:'Second',version:'1.20.1',loader:'fabric'}];return setupApp;}
 let app=init();await tick();let d=app.dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установить/);d.querySelector('[data-action=install-mod]').click();await tick();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);assert.equal(d.querySelector('[data-action=install-mod]'),null);assert.ok(d.querySelector('[data-action=my-mod]'));
 app.dom.window.close(); app=init();await tick();d=app.dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);d.querySelector('[data-action=my-mod]').click();await tick();d.querySelector('[data-action=delete-mod]').click();await tick();d.querySelector('[data-mod-tab=catalog]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установить/);
 await app.dom.window.__TAURI__.core.invoke('select_item',{id:'b'});d.querySelector('[data-page=profiles]').click();d.querySelector('[data-page=mods]').click(); // snapshot must be refreshed as in a real selection
 // Use the real select-profile action to refresh application state.
 d.querySelector('[data-page=profiles]').click(); const choose=d.querySelector('[data-action=select-profile][data-id=b]'); if(choose)choose.click();else {d.querySelector('[data-action=open-instance][data-id=b]').click();} await tick();d.querySelector('[data-page=mods]').click();await tick();d.querySelector('[data-action=install-mod]').click();await tick();await tick();assert.equal(files.a.length,0);assert.equal(files.b.length,1);
 files.b[0].update={id:'new'};d.querySelector('[data-mod-tab=catalog]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Обновить/);d.querySelector('[data-action=update-mod]').click();await tick();await tick();assert.equal(files.b[0].version.id,'new');assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);app.dom.window.close();
});

test('unknown or failed file identification never claims that a mod is absent',async()=>{
 const {dom}=setup({mod_status:async()=>({files:[],unknown:1,lookup_complete:true})});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Статус неизвестен/);assert.equal(d.querySelector('[data-action=install-mod]'),null);dom.window.close();
});
