// Only injected for the explicit --ui-smoke-test invocation in CI.
(async () => {
  const report=(success,details)=>window.__TAURI__.core.invoke('ui_smoke_result',{success,details});
  try {
    const assert=(test,message)=>{if(!test)throw new Error(message);};
    assert(window.HorizonDescription && window.DOMPurify,'renderer unavailable');
    const worker = new Worker('description-worker.js');
    const parsed = await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('worker timeout')),10000);worker.onmessage=e=>{clearTimeout(timer);resolve(e.data);};worker.onerror=()=>reject(new Error('worker error'));worker.postMessage({id:1,text:'# Worker\n\n**ok**'});});
    worker.terminate();assert(parsed.chunks?.join('').includes('<strong>ok</strong>'),'worker did not render Markdown');
    const dialog=document.querySelector('#mod-dialog'),scroll=document.querySelector('#mod-scroll'),body=document.querySelector('#mod-body');
    document.querySelector('#mod-loading').classList.add('hidden');document.querySelector('#mod-detail-content').classList.remove('hidden');document.querySelector('#mod-title').textContent='WebKit UI smoke';dialog.showModal();
    const text=Array.from({length:120},(_,i)=>`## Раздел ${i}\n\n**Текст**\n\n![alt](https://cdn.modrinth.com/horizon-ci-missing-${i}.png)\n\n| A | B |\n|---|---|\n| x | y |\n\n`).join('')+'\n<iframe src="https://www.youtube.com/embed/dQw4w9WgXcQ"></iframe>\n\n<script>window.pwned=1</script>';
    const start=performance.now();await HorizonDescription.render(body,text,{root:scroll});const renderMs=performance.now()-start;
    assert(body.querySelectorAll('h2').length===120,'missing headings');assert(body.querySelectorAll('table').length===120,'missing tables');assert(body.querySelectorAll('script,iframe').length===0 && !window.pwned,'unsafe markup');assert(body.querySelector('[data-video]'),'video was not deferred');
    const head=document.querySelector('.mod-dialog-head').getBoundingClientRect().top,count=body.querySelectorAll('.description-chunk').length;let last=performance.now(),max=0;
    for(let i=0;i<60;i++){await new Promise(requestAnimationFrame);const now=performance.now();max=Math.max(max,now-last);last=now;scroll.scrollTop=(scroll.scrollHeight-scroll.clientHeight)*i/59;}
    assert(document.querySelector('.mod-dialog-head').getBoundingClientRect().top===head,'header moved');assert(body.querySelectorAll('.description-chunk').length===count,'scroll rebuilt description');
    dialog.close();await report(true,JSON.stringify({engine:'Flatpak WebKitGTK under Xvfb',headings:120,images:120,renderMs:Math.round(renderMs),worstFrameMs:Math.round(max),chunks:count}));
  } catch(error) { await report(false,String(error?.stack||error)); }
})();
