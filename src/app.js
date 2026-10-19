let current=null;
const $=id=>document.getElementById(id);
const token=()=>sessionStorage.getItem('lark_api_key')||'';
$('key').value=token();
$('key').addEventListener('input',()=>sessionStorage.setItem('lark_api_key',$('key').value));
async function api(path, method='GET', body) {
 const h={'Authorization':'Bearer '+token()};
 if(body!==undefined) h['Content-Type']='application/json';
 const r=await fetch(path,{method,headers:h,body:body===undefined?undefined:JSON.stringify(body)});
 const data=await r.json().catch(()=>({error:'Invalid JSON response'}));
 if(!r.ok) throw Error(data.error||'HTTP '+r.status);
 return data;
}
const blockFields={2:'text',3:'heading1',4:'heading2',5:'heading3',12:'bullet',13:'ordered',15:'quote'};
function renderRichBlock(root,kind,content,style={}){
 const tags={text:'p',heading1:'h1',heading2:'h2',heading3:'h3',bullet:'li',ordered:'li',quote:'blockquote'};
 const tag=tags[kind];if(!tag)return;
 let element=document.createElement(tag);
 let target=element;
 if(style.bold){const bold=document.createElement('strong');target.append(bold);target=bold}
 if(style.italic){const italic=document.createElement('em');target.append(italic);target=italic}
 target.textContent=content;
 if(kind==='bullet'||kind==='ordered'){
  const listTag=kind==='bullet'?'ul':'ol';
  let list=root.lastElementChild;
  if(!list||list.tagName.toLowerCase()!==listTag){list=document.createElement(listTag);root.append(list)}
  list.append(element);
 }else root.append(element);
}
function renderDraft(){
 const root=$('draftPreview');root.replaceChildren();
 const text=$('structuredText').value;
 if(!text){const empty=document.createElement('span');empty.className='empty';empty.textContent='输入内容后可在这里预览';root.append(empty);return}
 renderRichBlock(root,$('kind').value,text,{bold:$('bold').checked,italic:$('italic').checked});
}
const status=msg=>$('status').textContent=msg;
async function list() {
 try { const result=await api('/api/docs');$('docs').replaceChildren();
 for (const d of result.documents) {const b=document.createElement('button');b.className='doc';b.textContent=d.title+(d.public_read?' · 公开只读':'');b.onclick=()=>selectDoc(d);$('docs').append(b);}
 status('已加载 '+result.documents.length+' 篇文档');
 }catch(e){status(e.message);}
}
async function selectDoc(doc) {
 if(current && current.id!==doc.id){
  if((queued.length || $('structuredText').value.trim()) && !confirm('切换文档将清空当前未提交的结构化草稿，是否继续？'))return;
  queued.length=0;$('structuredText').value='';renderDraft();
 }
 current=doc;$('heading').textContent=doc.title;$('append').disabled=false;$('refresh').disabled=false;
 $('public').checked=doc.public_read;$('adminControls').hidden=false;
 const a=document.createElement('a');a.href=doc.url;a.target='_blank';a.rel='noopener noreferrer';a.textContent='在飞书中打开 ↗';$('link').replaceChildren(a);
 renderQueued();await refresh();
}
let refreshSequence=0;
async function refresh() {
 if(!current)return;
 const docId=current.id, seq=++refreshSequence;
 try{const data=await api('/api/docs/'+encodeURIComponent(docId));
 if(seq!==refreshSequence||current?.id!==docId)return;
 $('preview').textContent=data.content||'(空文档)';
 await loadParagraphs(docId,seq);
 if(seq===refreshSequence&&current?.id===docId)status('已从飞书同步');
 }catch(e){if(seq===refreshSequence&&current?.id===docId)status(e.message)}
}
async function loadParagraphs(docId,seq=refreshSequence) {
 const root=$('paragraphs');root.replaceChildren();
 const visual=$('visualBlocks');visual.replaceChildren();
 const items=[];
 let cursor='', more=true, pages=0;
 const seen=new Set();
 while(more && pages<10){
  if(seq!==refreshSequence||!current || current.id!==docId)return;
  const path='/api/docs/'+encodeURIComponent(docId)+'/blocks'+(cursor?'?page_token='+encodeURIComponent(cursor):'');
  const response=await api(path);
  if(Array.isArray(response.items))items.push(...response.items);
  pages++;
  more=response.has_more===true;
  if(more){
   if(typeof response.page_token!=='string'||!response.page_token||seen.has(response.page_token)){
    root.textContent='分页数据异常，请在飞书中查看。';return;
   }
   seen.add(response.page_token);cursor=response.page_token;
  }
 }
 if(seq!==refreshSequence||!current||current.id!==docId)return;
 let count=0;
 for(const block of items){
  const field=blockFields[block.block_type];
  if(field&&Array.isArray(block[field]?.elements)){
   const runs=block[field].elements;
   const plain=runs.every(e=>typeof e.text_run?.content==='string');
   if(plain){
    const text=runs.map(e=>e.text_run.content).join('');
    const styles=runs.length===1?(runs[0].text_run.text_element_style||{}):{};
    renderRichBlock(visual,field,text,styles);
    if(runs.length>1){
     const element=visual.lastElementChild;
     const target=(field==='bullet'||field==='ordered')?element?.lastElementChild:element;
     if(target){
      target.replaceChildren();
      for(const run of runs){
       const style=run.text_run.text_element_style||{};
       let node=target;
       if(style.bold){const bold=document.createElement('strong');node.append(bold);node=bold}
       if(style.italic){const italic=document.createElement('em');node.append(italic);node=italic}
       const span=document.createElement('span');span.textContent=run.text_run.content;node.append(span);
      }
     }
    }
   } else {
    const note=document.createElement('p');note.className='empty';
    note.textContent='[此内容包含不支持的嵌入元素]';visual.append(note);
   }
  }

  if(!field || !block.block_id || !Array.isArray(block[field]?.elements)) continue;
  const elements=block[field].elements;
  if(elements.length===0 || elements.length>100 || elements.some(e=>typeof e.text_run?.content!=='string'||Object.keys(e).length!==1)) continue;
  const original=elements.map(e=>e.text_run.content).join('');
  const wrap=document.createElement('div');wrap.className='block-editor';
  const caption=document.createElement('label');caption.textContent=field+' · 内容块 '+(++count)+(elements.length>1?' · '+elements.length+' 个文本片段':'');
  const areas=[],formatStates=[];
  const runInputs=document.createElement('div');
  const runPreview=document.createElement('div');runPreview.className='rich-preview';
  const updateRunPreview=()=>{
   runPreview.replaceChildren();
   for(let i=0;i<areas.length;i++){
    const span=document.createElement('span');
    span.textContent=areas[i].value;
    let node=span;
    if(formatStates[i].italic){const em=document.createElement('em');em.append(node);node=em}
    if(formatStates[i].bold){const strong=document.createElement('strong');strong.append(node);node=strong}
    runPreview.append(node);
   }
  };
  for(let i=0;i<elements.length;i++){
   const item=elements[i].text_run;
   const label=document.createElement('label');label.textContent='片段 '+(i+1);
   const styles=item.text_element_style||{};
   const formatting={bold:styles.bold===true,italic:styles.italic===true};
   formatStates.push(formatting);
   const fieldArea=document.createElement('textarea');fieldArea.value=item.content;fieldArea.rows=2;fieldArea.maxLength=20000;
   fieldArea.addEventListener('input',updateRunPreview);
   const controls=document.createElement('div');controls.className='editor-toolbar';
   for(const [key,title] of [['bold','B 加粗'],['italic','I 斜体']]){
    const toggle=document.createElement('button');toggle.type='button';toggle.className='secondary';toggle.textContent=title;
    toggle.setAttribute('aria-pressed',String(formatting[key]));
    toggle.onclick=()=>{
     formatting[key]=!formatting[key];
     toggle.setAttribute('aria-pressed',String(formatting[key]));
     updateRunPreview();
    };
    controls.append(toggle);
   }
   areas.push(fieldArea);runInputs.append(label,fieldArea,controls);
  }
  updateRunPreview();
  const previewTitle=document.createElement('label');previewTitle.textContent='本段落修改后预览';
  runInputs.append(previewTitle,runPreview);
  const button=document.createElement('button');button.className='secondary';button.textContent='保存该段落';
  button.onclick=async()=>{
   if(!current || current.id!==docId)return status('文档已切换，请刷新');
   const edits=areas.map((area,i)=>({expected_text:elements[i].text_run.content,text:area.value,bold:formatStates[i].bold,italic:formatStates[i].italic}));
   if(edits.every((item,i)=>item.text===item.expected_text&&item.bold===(elements[i].text_run.text_element_style?.bold===true)&&item.italic===(elements[i].text_run.text_element_style?.italic===true)))return status('内容没有变化');
   if(edits.reduce((sum,item)=>sum+Array.from(item.text).length,0)>20000)return status('整个内容块不能超过 20000 字符');
   if(!confirm('确认修改 '+edits.length+' 个文本片段？将按预览同步文字与加粗、斜体设置。'))return;
   button.disabled=true;
   try {
    await api('/api/docs/'+encodeURIComponent(docId)+'/blocks/'+encodeURIComponent(block.block_id)+'/runs','PATCH',{runs:edits});
    await refresh();status('文本片段修改成功');
   }catch(e){status((e.message.includes('changed')?'内容已由其他人修改，请刷新后重新编辑。':'保存失败：'+e.message));}
   finally {button.disabled=false;}
  };
  wrap.append(caption,runInputs,button);root.append(wrap);
 }
 if(more){const note=document.createElement('p');note.className='note';note.textContent='仅显示前 '+pages+' 页内容，超出范围请在飞书中查看。';root.prepend(note)}
 if(count===0)root.textContent='当前没有可编辑的纯文本 Run 内容块。';
 if(!visual.hasChildNodes())visual.textContent='没有可预览的支持内容。';
}
const queued=[];
function renderQueued(){
 const root=$('queuedBlocks');root.replaceChildren();
 for(let i=0;i<queued.length;i++){
  const item=queued[i];const div=document.createElement('div');div.className='block-editor';
  const label=document.createElement('div');label.className='note';label.textContent='待提交内容 '+(i+1)+' · '+item.kind;
  const preview=document.createElement('div');preview.className='rich-preview';
  renderRichBlock(preview,item.kind,item.text,item);
  const remove=document.createElement('button');remove.className='secondary';remove.textContent='移除';
  remove.onclick=()=>{queued.splice(i,1);renderQueued()};
  div.append(label,preview,remove);root.append(div);
 }
 $('submitBlocks').disabled=!current||queued.length===0;
}
for(const id of ['kind','bold','italic','structuredText']){
 $(id).addEventListener(id==='structuredText'?'input':'change',renderDraft);
}
for(const [id,kind] of Object.entries({useH1:'heading1',useH2:'heading2',useBullet:'bullet',useQuote:'quote'})){
 $(id).onclick=()=>{$('kind').value=kind;renderDraft();$('structuredText').focus()};
}
$('useBold').onclick=()=>{$('bold').checked=!$('bold').checked;renderDraft()};
$('useItalic').onclick=()=>{$('italic').checked=!$('italic').checked;renderDraft()};
renderDraft();
$('queueBlock').onclick=()=>{
 const text=$('structuredText').value.trim();if(!text)return status('请输入内容');
 queued.push({kind:$('kind').value,text,bold:$('bold').checked,italic:$('italic').checked});
 $('structuredText').value='';renderDraft();renderQueued();status('已加入待提交列表');
};
$('clearBlocks').onclick=()=>{queued.length=0;renderQueued()};
$('submitBlocks').onclick=async()=>{
 if(!current||!queued.length)return;
 const docId=current.id;const blocks=queued.map(b=>({...b}));
 if(!confirm('向当前飞书文档追加 '+blocks.length+' 个内容块？'))return;
 $('submitBlocks').disabled=true;
 try{
  await api('/api/docs/'+encodeURIComponent(docId)+'/append-structured','POST',{blocks});
  queued.length=0;renderQueued();await refresh();status('结构化内容已提交');
 }catch(e){status('提交失败：'+e.message);renderQueued()}
};
$('load').onclick=list;
$('create').onclick=async()=>{
 try {const d=await api('/api/docs','POST',{title:$('title').value.trim()});$('title').value='';await list();await selectDoc(d);}
 catch(e){status(e.message);}
};
$('append').onclick=async()=>{
 if(!current)return;
 const text=$('text').value;if(!text.trim())return status('请输入正文');
 try{await api('/api/docs/'+encodeURIComponent(current.id)+'/append','POST',{text});
 $('text').value='';await refresh();status('追加成功');}catch(e){status(e.message);}
};
$('refresh').onclick=refresh;
$('saveAccess').onclick=async()=>{
 if(!current)return;
 try {await api('/api/docs/'+encodeURIComponent(current.id)+'/access','POST',{public_read:$('public').checked});
 current.public_read=$('public').checked;await list();status('访问控制已更新');}catch(e){status(e.message);}
};
