// Optional real-browser UI/performance smoke. Run with PLAYWRIGHT_CHROMIUM_EXECUTABLE and playwright-core installed.
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');
(async () => {
 const browser=await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE || '/usr/bin/google-chrome',args:['--no-sandbox']});
 const page=await browser.newPage({viewport:{width:1280,height:800},reducedMotion:'reduce'});
 let requests=0,blocked=0;await page.route('https://evil.example/**',route=>{blocked++;return route.abort();});const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a9sAAAAASUVORK5CYII=','base64');
 await page.route('https://cdn.modrinth.com/**',route=>{requests++;return route.fulfill({status:200,contentType:'image/png',body:png});});
 await page.addInitScript(()=>{
  const body=Array.from({length:220},(_,i)=>`## Раздел ${i}\n\nДлинный **текст** и ссылка [документация](https://example.com).\n\n![Картинка ${i}](https://cdn.modrinth.com/test/${i}.png)\n\n| A | B |\n|---|---|\n| x | y |\n\n- one\n- two\n\n`).join('')+'\n<iframe src="https://www.youtube.com/embed/dQw4w9WgXcQ"></iframe>\n<iframe src="https://evil.example/test"></iframe>\n<script>window.pwned=1</script>';
  window.__TAURI__={core:{invoke:async(name,args)=>{if(name==='snapshot')return {accounts:[{id:'a',name:'Steve'}],profiles:[{id:'p',name:'Test',version:'1.20.1',loader:'fabric',mod_count:1}],active_profile:'p',active_account:'a',settings:{java:'auto',memory_mb:2048}};if(name==='profile_status')return {state:'ready',path:'/game'};if(name==='catalog_search')return {hits:[{project_id:'mod1',title:'Test Mod',author:'Dev',description:'A mod',downloads:42,versions:['1.20.1'],categories:['fabric']}],total_hits:1};if(name==='mod_status')return {files:[],lookup_complete:true,unknown:0};if(name==='catalog_categories')return [];if(name==='catalog_details')return {project:{id:'mod1',title:'Test Mod',description:'A mod',body,game_versions:['1.20.1'],loaders:['fabric']},members:[],compatible_version:{version_number:'1',dependencies:[]},available_versions:[]};return null;}},event:{listen:async()=>{}}};
  window.perfLong=[];try{new PerformanceObserver(list=>window.perfLong.push(...list.getEntries().map(e=>({start:Math.round(e.startTime),duration:Math.round(e.duration)})))).observe({type:'longtask',buffered:true});}catch(_){}
 });
 const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(process.env.HORIZON_TEST_URL||'http://127.0.0.1:4173/');
 await page.locator('[data-page=mods]').click();await page.locator('[data-action=view-mod]').first().waitFor();
 await page.locator('.content').evaluate(e=>e.scrollTop=120);await page.locator('[data-action=view-mod]').first().scrollIntoViewIfNeeded();const before=await page.locator('.content').evaluate(e=>e.scrollTop);
 const perfStart=await page.evaluate(()=>performance.now());const started=Date.now();await page.locator('[data-action=view-mod]').first().click();
 await page.locator('#mod-body .description-chunk').last().waitFor();
 await page.waitForFunction(()=>document.querySelectorAll('#mod-body h2').length===220,{timeout:30000});
 const renderMs=Date.now()-started;const initialRequests=requests;
 assert(initialRequests<50,`Images loaded eagerly: ${initialRequests}`);
 assert.equal(await page.locator('#mod-body script, #mod-body iframe').count(),0);
 assert.equal(await page.evaluate(()=>window.pwned),undefined);assert.equal(blocked,0);
 assert.equal(await page.locator('#mod-body table').count(),220);
 assert.equal(await page.locator('#mod-body [data-video]').count(),1);
 const scrollResult=await page.locator('#mod-scroll').evaluate(async scroller=>{
  const head=document.querySelector('.mod-dialog-head').getBoundingClientRect().top;const nodes=document.querySelectorAll('#mod-body .description-chunk').length;const deltas=[];let last=performance.now();
  for(let i=0;i<80;i++){await new Promise(requestAnimationFrame);const now=performance.now();deltas.push(now-last);last=now;scroller.scrollTop=(scroller.scrollHeight-scroller.clientHeight)*i/79;}
  return {headBefore:head,headAfter:document.querySelector('.mod-dialog-head').getBoundingClientRect().top,nodesBefore:nodes,nodesAfter:document.querySelectorAll('#mod-body .description-chunk').length,worstMs:Math.max(...deltas),p95Ms:deltas.sort((a,b)=>a-b)[Math.floor(deltas.length*.95)]};
 });
 assert.equal(scrollResult.headBefore,scrollResult.headAfter);assert.equal(scrollResult.nodesBefore,scrollResult.nodesAfter);
 await page.keyboard.press('Escape');await page.locator('#mod-dialog').waitFor({state:'hidden'});assert.equal(await page.locator('.content').evaluate(e=>e.scrollTop),before);
 await page.locator('[data-action=view-mod]').first().click();await page.locator('#mod-body h2').first().waitFor();await page.locator('#close-dialog').click();await page.locator('#mod-dialog').waitFor({state:'hidden'});
 await page.locator('[data-page=home]').click();await page.locator('[data-page=profiles]').click();await page.locator('[data-action=open-instance]').first().click();await page.locator('#instance.active').waitFor();
 assert.deepEqual(errors,[]);
 console.log('::notice title=Horizon browser performance::'+JSON.stringify({renderMs,images:220,initialImageRequests:initialRequests,requestsAfterScroll:requests,scroll:scrollResult,longTasks:await page.evaluate(start=>window.perfLong.filter(e=>e.start>=start),perfStart)}));
 await browser.close();
})().catch(e=>{console.error(e);process.exit(1)});
