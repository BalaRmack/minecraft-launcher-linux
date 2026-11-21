'use strict';
importScripts('vendor/marked.umd.js');
self.onmessage = ({data}) => {
  try {
    const tokens = marked.lexer(data.text, {gfm:true, breaks:true});
    const chunks = []; let group = [], size = 0;
    const flush = () => { if (!group.length) return; group.links=tokens.links; chunks.push(marked.parser(group,{gfm:true,breaks:true})); group=[];size=0; };
    for (const token of tokens) { if(size>600)flush();group.push(token);size+=(token.raw||'').length; }
    flush(); self.postMessage({id:data.id,chunks});
  } catch(error) { self.postMessage({id:data.id,error:String(error)}); }
};
