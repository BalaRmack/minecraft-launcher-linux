/* Modrinth bodies are untrusted. Markdown is parsed off-thread; every rendered
   fragment is sanitized, and external media is opt-in/lazy. */
(function () {
  'use strict';
  const cache = new Map(), pending = new Map(); let worker, nextId=0;
  const MAX_SOURCE = 600000, MAX_CACHE = 4;
  function chunksOnMain(text) {
    const tokens=marked.lexer(text,{gfm:true,breaks:true}),chunks=[];let group=[],size=0;
    const flush=()=>{if(!group.length)return;group.links=tokens.links;chunks.push(marked.parser(group,{gfm:true,breaks:true}));group=[];size=0;};
    for(const token of tokens){if(size>6000)flush();group.push(token);size+=(token.raw||'').length;}flush();return chunks;
  }
  function parse(text) {
    if(cache.has(text)){const value=cache.get(text);cache.delete(text);cache.set(text,value);return Promise.resolve(value);}
    const finish=chunks=>{cache.set(text,chunks);while(cache.size>MAX_CACHE)cache.delete(cache.keys().next().value);return chunks;};
    if(typeof Worker==='undefined')return Promise.resolve().then(()=>finish(chunksOnMain(text)));
    try {
      if(!worker){worker=new Worker('description-worker.js');worker.onmessage=({data})=>{const item=pending.get(data.id);if(!item)return;pending.delete(data.id);data.error?item.reject(new Error(data.error)):item.resolve(data.chunks);};worker.onerror=()=>{for(const item of pending.values())item.reject(new Error('Не удалось обработать описание'));pending.clear();worker.terminate();worker=null;};}
      const id=++nextId;return new Promise((resolve,reject)=>{pending.set(id,{resolve,reject});worker.postMessage({id,text});}).then(finish).catch(()=>finish(chunksOnMain(text)));
    } catch(e){return Promise.resolve().then(()=>finish(chunksOnMain(text)));}
  }
  function url(raw,base) { if(typeof raw!=='string'||!raw.trim())return null; try { const u=new URL(raw,base); return u.protocol==='https:' && !u.username && !u.password ? u.href : null; } catch(_){return null;} }
  function video(raw) {
    try {const u=new URL(raw);if(u.protocol!=='https:')return null;
      if(['www.youtube.com','youtube.com','www.youtube-nocookie.com','youtube-nocookie.com'].includes(u.hostname)){const id=u.pathname.match(/^\/embed\/([A-Za-z0-9_-]{11})\/?$/);return id?`https://www.youtube-nocookie.com/embed/${id[1]}`:null;}
      if(u.hostname==='player.vimeo.com'){const id=u.pathname.match(/^\/video\/(\d+)\/?$/);return id?`https://player.vimeo.com/video/${id[1]}`:null;}
    }catch(_){}return null;
  }
  function imageNode(source,alt,observer) {
    const wrap=document.createElement('span');wrap.className='description-image loading';
    const img=document.createElement('img');img.alt=alt||'Изображение из описания';img.loading='lazy';img.decoding='async';img.dataset.src=source;
    img.addEventListener('load',()=>wrap.classList.remove('loading'),{once:true});
    img.addEventListener('error',()=>{wrap.classList.remove('loading');wrap.classList.add('failed');img.remove();wrap.textContent='Изображение недоступно';},{once:true});wrap.append(img);
    if(observer)observer.observe(img);else img.src=source;
    return wrap;
  }
  function sanitize(html,base,observer) {
    const fragment=DOMPurify.sanitize(html,{RETURN_DOM_FRAGMENT:true, ALLOWED_TAGS:['p','br','hr','h1','h2','h3','h4','h5','h6','strong','b','em','i','del','s','ul','ol','li','blockquote','pre','code','a','img','table','thead','tbody','tr','th','td','center','div','span','details','summary','sup','sub','iframe'], ALLOWED_ATTR:['href','src','alt','title'], ALLOW_DATA_ATTR:false, FORBID_TAGS:['style','script','svg','math','form','input','object','embed','video','audio']});
    fragment.querySelectorAll('a').forEach(a=>{const href=url(a.getAttribute('href'),base);if(href){a.href=href;a.rel='noopener noreferrer';a.dataset.external=href;}else a.replaceWith(...a.childNodes);});
    fragment.querySelectorAll('img').forEach(img=>{const src=url(img.getAttribute('src'),base);if(!src){const placeholder=document.createElement('span');placeholder.className='description-image failed';placeholder.textContent='Изображение недоступно';img.replaceWith(placeholder);}else img.replaceWith(imageNode(src,img.getAttribute('alt'),observer));});
    fragment.querySelectorAll('iframe').forEach(frame=>{const src=video(frame.getAttribute('src'));if(!src){const note=document.createElement('p');note.className='embed-note';note.textContent='Встроенный материал недоступен: сервис не разрешён.';frame.replaceWith(note);return;}const button=document.createElement('button');button.type='button';button.className='video-load';button.textContent='▶  Загрузить видео';button.dataset.video=src;frame.replaceWith(button);});
    return fragment;
  }
  function startVideo(button) { const src=video(button.dataset.video);if(!src)return;const frame=document.createElement('iframe');frame.src=src;frame.loading='lazy';frame.title='Видео из описания мода';frame.setAttribute('sandbox','allow-scripts allow-same-origin allow-presentation');frame.setAttribute('allow','fullscreen');frame.setAttribute('allowfullscreen','');frame.referrerPolicy='no-referrer';frame.className='description-video';button.replaceWith(frame); }
  async function render(container, text, options={}) {
    const token=Symbol();container._renderToken=token;container.replaceChildren();if(container._imageObserver)container._imageObserver.disconnect();
    const observer=typeof IntersectionObserver==='function'?new IntersectionObserver(entries=>{for(const entry of entries){if(entry.isIntersecting){const img=entry.target;observer.unobserve(img);if(img.dataset.src){img.src=img.dataset.src;delete img.dataset.src;}}}},{root:options.root||null,rootMargin:'350px'}):null;container._imageObserver=observer;
    const truncated=text.length>MAX_SOURCE; const source=text.slice(0,MAX_SOURCE);
    const chunks=await parse(source);if(container._renderToken!==token)return;
    let i=0;
    await new Promise(resolve=>{function step(){if(container._renderToken!==token){resolve();return;}const start=performance.now();let count=0;while(i<chunks.length&&count<4&&performance.now()-start<8){const section=document.createElement('section');section.className='description-chunk';section.append(sanitize(chunks[i++],options.base||'https://modrinth.com/',observer));container.append(section);count++;}if(i<chunks.length)(window.requestAnimationFrame||setTimeout)(step);else resolve();}step();});
    if(truncated&&container._renderToken===token){const note=document.createElement('p');note.className='embed-note';note.textContent='Описание слишком длинное; показана только первая часть.';container.append(note);}
  }
  function clear(container){container._renderToken=Symbol();if(container._imageObserver)container._imageObserver.disconnect();container.replaceChildren();}
  window.HorizonDescription={render,clear,startVideo,imageNode};
})();
