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
  dom.window.__TAURI__ = {core:{invoke:async (name,args) => {calls.push([name,args]); if (overrides[name]) return overrides[name](args); if(name==='snapshot')return snapshot; if(name==='profile_status')return {state:'ready',path:'/game'}; if(name==='versions')return [{id:'1.21.4',type:'release'},{id:'25w01a',type:'snapshot'},{id:'old',type:'old_beta'}]; if(name==='list_mods'||name==='inspect_mods'||name==='catalog_categories')return []; if(name==='catalog_search')return {hits:[{project_id:'abc',title:'Example',description:'mod',author:'Author',downloads:10,versions:['1.21.4'],categories:['fabric']}],total_hits:21}; return null;}},event:{listen:async (name, callback)=>{listeners[name]=callback;}}};
  dom.window.eval(fs.readFileSync('ui/vendor/marked.umd.js','utf8'));
  dom.window.eval(fs.readFileSync('ui/vendor/purify.min.js','utf8'));
  dom.window.eval(fs.readFileSync('ui/description.js','utf8'));
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
 const opts={catalog_details:async()=>({project:{id:'xaero',title:"Xaero's Minimap",body:'Overview'},compatible_version:{id:'compatible',dependencies:[]}}),catalog_search:async()=>({hits:[{project_id:'xaero',title:"Xaero's Minimap",downloads:10,versions:['1.20.1'],categories:['fabric']}],total_hits:1}),mod_status:async({id})=>({files:files[id].map(x=>({...x})),lookup_complete:true,unknown:0}),install_mod:async({id,update})=>{files[id]=[{...installed,version:{...installed.version,id:update?'new':'old'}}];return 1;},list_mods:async({id})=>files[id].map(x=>({name:x.name,enabled:x.enabled})),inspect_mods:async({id})=>files[id].map(x=>({...x,project:{id:'xaero',title:"Xaero's Minimap"},warnings:[]})),change_mod:async({id})=>{files[id]=[];},select_item:async({id})=>{setupApp.snapshot.active_profile=id;}};
 function init(){setupApp=setup(opts);setupApp.snapshot.active_profile='a';setupApp.snapshot.profiles=[{id:'a',name:'First',version:'1.20.1',loader:'fabric'},{id:'b',name:'Second',version:'1.20.1',loader:'fabric'}];return setupApp;}
 let app=init();await tick();let d=app.dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Проверить совместимость/);d.querySelector('[data-action=view-mod]').click();await tick();d.querySelector('#detail-install').click();await tick();await tick();d.querySelector('#close-dialog').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);assert.equal(d.querySelector('[data-action=install-mod]'),null);assert.ok(d.querySelector('[data-action=my-mod]'));
 app.dom.window.close(); app=init();await tick();d=app.dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);d.querySelector('[data-action=my-mod]').click();await tick();d.querySelector('[data-action=delete-mod]').click();await tick();d.querySelector('[data-mod-tab=catalog]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Проверить совместимость/);
 await app.dom.window.__TAURI__.core.invoke('select_item',{id:'b'});d.querySelector('[data-page=profiles]').click();d.querySelector('[data-page=mods]').click(); // snapshot must be refreshed as in a real selection
 // Use the real select-profile action to refresh application state.
 d.querySelector('[data-page=profiles]').click(); const choose=d.querySelector('[data-action=select-profile][data-id=b]'); if(choose)choose.click();else {d.querySelector('[data-action=open-instance][data-id=b]').click();} await tick();d.querySelector('[data-page=mods]').click();await tick();d.querySelector('[data-action=view-mod]').click();await tick();d.querySelector('#detail-install').click();await tick();await tick();d.querySelector('#close-dialog').click();await tick();assert.equal(files.a.length,0);assert.equal(files.b.length,1);
 files.b[0].update={id:'new'};d.querySelector('[data-mod-tab=catalog]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Обновить/);d.querySelector('[data-action=update-mod]').click();await tick();await tick();assert.equal(files.b[0].version.id,'new');assert.match(d.querySelector('.mod-card-footer').textContent,/Установлено/);app.dom.window.close();
});

test('unknown or failed file identification never claims that a mod is absent',async()=>{
 const {dom}=setup({mod_status:async()=>({files:[],unknown:1,lookup_complete:true})});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Статус неизвестен/);assert.equal(d.querySelector('[data-action=install-mod]'),null);dom.window.close();
});

test('renders Markdown, allowed HTML, lazy images and safe opt-in videos without executable markup', async()=>{
 const {dom}=setup();await tick();const d=dom.window.document,body=d.querySelector('#mod-body');
 const text='# Заголовок\n\n**жирный** и *курсив*\n\n- один\n- два\n\n| A | B |\n|---|---|\n| x | y |\n\n> цитата\n\n```js\nlet x = 1;\n```\n\n<center><b>HTML</b></center>\n\n<img src="https://cdn.modrinth.com/image.png" onerror="window.pwned=1">\n\n<script>window.pwned=2</script><svg onload="window.pwned=3"></svg>\n\n[bad](javascript:alert(1))\n\n<iframe src="https://www.youtube.com/embed/dQw4w9WgXcQ" onload="alert(1)"></iframe>\n\n<iframe src="https://evil.example/embed/test"></iframe>';
 await dom.window.HorizonDescription.render(body,text,{base:'https://modrinth.com/'});
 assert.equal(body.querySelector('h1').textContent,'Заголовок');assert.ok(body.querySelector('strong'));assert.ok(body.querySelector('em'));assert.equal(body.querySelectorAll('li').length,2);assert.ok(body.querySelector('table'));assert.ok(body.querySelector('blockquote'));assert.ok(body.querySelector('pre code'));assert.ok(body.querySelector('center b'));assert.equal(body.querySelectorAll('script,svg,iframe').length,0);assert.equal(body.querySelectorAll('[onerror],[onload]').length,0);assert.equal(dom.window.pwned,undefined);assert.equal(body.querySelector('a[href^="javascript:"]'),null);assert.equal(body.querySelectorAll('[data-video]').length,1);const btn=body.querySelector('[data-video]');dom.window.HorizonDescription.startVideo(btn);const frame=body.querySelector('iframe');assert.equal(frame.getAttribute('src'),'https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ');assert.equal(frame.getAttribute('sandbox'),'allow-scripts allow-same-origin allow-presentation');assert.match(body.textContent,/сервис не разрешён/);dom.window.close();
});
test('mod dialog displays loading, retry, keyboard close and keeps catalog state',async()=>{
 let tries=0;const {dom,calls}=setup({mod_status:async()=>({files:[],unknown:0,lookup_complete:true}),catalog_details:async()=>{tries++;if(tries===1)throw Error('offline');return {project:{id:'abc',title:'Example',description:'Summary',body:'# Heading\n\nA **mod**',loaders:['fabric'],game_versions:['1.21.4']},members:[{user:{username:'Alex'}}],compatible_version:{version_number:'1',dependencies:[]},available_versions:[{id:'v1',version_number:'1'}]};}});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();const form=d.querySelector('#mod-search');form.elements.query.value='example';form.dispatchEvent(new dom.window.Event('submit',{bubbles:true,cancelable:true}));await tick();const scroll=d.querySelector('.content');scroll.scrollTop=240;d.querySelector('[data-action=view-mod]').click();await tick();assert.equal(d.querySelector('#mod-dialog').open,true);assert.equal(d.querySelector('#mod-error').classList.contains('hidden'),false);d.querySelector('#retry-mod').click();await tick();await tick();assert.equal(d.querySelector('#mod-title').textContent,'Example');assert.equal(d.querySelector('#mod-body h1').textContent,'Heading');assert.equal(d.querySelector('#mod-body strong').textContent,'mod');assert.match(d.querySelector('#mod-versions').textContent,/Совместимые версии/);assert.equal(d.querySelector('#mod-dialog').getAttribute('aria-labelledby'),'mod-title');d.querySelector('#mod-dialog').dispatchEvent(new dom.window.Event('cancel',{cancelable:true}));await tick();assert.equal(d.querySelector('#mod-dialog').open,false);assert.equal(scroll.scrollTop,240);assert.equal(form.elements.query.value,'example');assert.equal(calls.filter(x=>x[0]==='catalog_details').length,2);dom.window.close();
});
test('dialog ignores stale response and disables incompatible catalog installation',async()=>{
 let resolveFirst;const {dom}=setup({mod_status:async()=>({files:[],unknown:0,lookup_complete:true}),catalog_search:async()=>({hits:[{project_id:'one',title:'One',versions:['1.20'],categories:['forge']},{project_id:'two',title:'Two',versions:['1.21.4'],categories:['fabric']}],total_hits:2}),catalog_details:async({project})=>project==='one'?new Promise(r=>resolveFirst=r):({project:{id:'two',title:'Two',body:'Second'},compatible_version:{dependencies:[]}})});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card').textContent,/Несовместим/);d.querySelector('[data-action=view-mod][data-id=one]').click();await tick();d.querySelector('#close-dialog').click();await tick();d.querySelector('[data-action=view-mod][data-id=two]').click();await tick();resolveFirst({project:{id:'one',title:'Wrong',body:'First'}});await tick();assert.equal(d.querySelector('#mod-title').textContent,'Two');dom.window.close();
});
test('long description renders incrementally, defers images, and does not rebuild on scroll',async()=>{
 const {dom}=setup();await tick();const observed=[];dom.window.IntersectionObserver=class{constructor(cb){this.cb=cb}observe(el){observed.push(el)}unobserve(){}disconnect(){}};const d=dom.window.document,body=d.querySelector('#mod-body');const text=Array.from({length:130},(_,i)=>`## section ${i}\n\nparagraph ${'word '.repeat(30)}\n\n![alt](https://cdn.modrinth.com/${i}.png)\n\n`).join('');await dom.window.HorizonDescription.render(body,text);assert.equal(body.querySelectorAll('h2').length,130);assert.equal(observed.length,130);assert.equal(body.querySelectorAll('img[src]').length,0);const first=body.firstChild;d.querySelector('#mod-scroll').dispatchEvent(new dom.window.Event('scroll'));assert.equal(body.firstChild,first);assert.ok(body.querySelectorAll('.description-chunk').length>1);dom.window.close();
});
test('quick start reflects current ready profile and mod dialog cannot close during install',async()=>{
 let resolve;const {dom}=setup({mod_status:async()=>({files:[],unknown:0,lookup_complete:true}),catalog_details:async()=>({project:{id:'abc',title:'Example',body:'hello'},compatible_version:{id:'v',dependencies:[]}}),install_mod:()=>new Promise(r=>resolve=r)});await tick();const d=dom.window.document;assert.match(d.querySelector('#quick-title').textContent,/готово/);assert.match(d.querySelector('#quick-description').textContent,/1.21.4/);assert.ok(d.querySelector('.home-cover'));d.querySelector('[data-page=mods]').click();await tick();d.querySelector('[data-action=view-mod]').click();await tick();d.querySelector('#detail-install').click();await tick();d.querySelector('#close-dialog').click();await tick();assert.equal(d.querySelector('#mod-dialog').open,true);resolve(1);await tick();await tick();d.querySelector('#close-dialog').click();await tick();assert.equal(d.querySelector('#mod-dialog').open,false);dom.window.close();
});
test('catalog filters and scroll position survive navigation to another section and back',async()=>{
 const {dom}=setup({mod_status:async()=>({files:[],lookup_complete:true,unknown:0})});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();const form=d.querySelector('#mod-search');form.elements.query.value='minimap';form.dispatchEvent(new dom.window.Event('submit',{bubbles:true,cancelable:true}));await tick();const scroller=d.querySelector('.content');scroller.scrollTop=170;d.querySelector('[data-page=home]').click();d.querySelector('[data-page=mods]').click();await tick();assert.equal(scroller.scrollTop,170);assert.equal(form.elements.query.value,'minimap');dom.window.close();
});
test('catalog does not infer a version-loader pair from independent search facets',async()=>{
 const {dom}=setup({mod_status:async()=>({files:[],unknown:0,lookup_complete:true}),catalog_details:async()=>({project:{id:'abc',title:'Example',body:'No matching release'},compatible_version:null})});await tick();const d=dom.window.document;d.querySelector('[data-page=mods]').click();await tick();assert.match(d.querySelector('.mod-card-footer').textContent,/Проверить совместимость/);assert.equal(d.querySelector('[data-action=install-mod]'),null);d.querySelector('[data-action=view-mod]').click();await tick();assert.equal(d.querySelector('#detail-install').disabled,true);assert.match(d.querySelector('#detail-install').textContent,/Нет совместимой версии/);dom.window.close();
});
