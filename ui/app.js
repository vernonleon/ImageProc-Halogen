'use strict';
const $=id=>document.getElementById(id);
const native=!!window.__TAURI__;
const invoke=native?window.__TAURI__.core.invoke:async()=>{throw Error('Open the ImageProc Halogen desktop app to use this feature.');};
const fields=['baseUrl','modelId','maxTokens','effort','maxThinkingTokens','samplingPreset','temperature','topP','topK','presencePenalty','repetitionPenalty','budget','concurrency','cacheMode','instruction','referenceInstruction'];
let imageBudgets={},references=[],referenceVersion=0;
let queuedPaths=new Set(),reviewShownLocked=false;
let items=[],selected=new Set(),page=0,currentPath=null,running=false,pauseRequested=false,stopCurrent=false,cancelRequests=new Map(),saveTimer,saveChain=Promise.resolve(),reviewVersion=0,gridVersion=0,prepareQueue=Promise.resolve(),toastTimer;
const preparedRefCache=new Map(),thumbCache=new Map(),reviewCache=new Map();
const PREVIEW_CONCURRENCY=4;
let previewActive=0;const previewPending=[];
const PAGE_SIZE=36;
const demo=new URLSearchParams(location.search).has('demo');

function message(text){$('toast').textContent=text;$('toast').hidden=false;clearTimeout(toastTimer);toastTimer=setTimeout(()=>$('toast').hidden=true,9000);}
function error(e){message(String(e?.message||e));}
function settings(){return Object.fromEntries(fields.map(k=>[k,$(k).value]));}
function applySettings(s){
  if(!s)return;
  for(const k of fields)if(s[k]!=null)$(k).value=s[k];
  if(!['no_store','fork','standard'].includes($('cacheMode').value))$('cacheMode').value='no_store';
  if($('effort').value==='none-qwen')$('effort').value='none';
  syncThinking();syncSampling();syncCacheMode();
}
function save(){clearTimeout(saveTimer);$('saveState').textContent='Saving…';saveTimer=setTimeout(()=>persist().catch(error),350);}
function persist(){clearTimeout(saveTimer);const data={version:1,items,settings:settings(),imageBudgets,references};const snapshot=JSON.parse(JSON.stringify(data));saveChain=saveChain.catch(()=>{}).then(async()=>{if(native)await invoke('write_recovery',{data:snapshot});else localStorage.setItem('imageproc-halogen-preview',JSON.stringify(snapshot));$('saveState').textContent='Saved locally';});return saveChain.catch(e=>{$('saveState').textContent='Save failed';throw e;});}
function tab(name){document.querySelectorAll('.panel').forEach(el=>el.classList.toggle('active',el.id===name));document.querySelectorAll('nav button').forEach(el=>el.classList.toggle('active',el.dataset.tab===name));if(name==='review')renderReview();if(name==='images')renderGrid();}
document.querySelectorAll('nav button').forEach(b=>b.onclick=()=>tab(b.dataset.tab));

function counts(){$('regenerateSelected').disabled=running||selected.size===0;for(const [id,status] of [['pendingCount','pending'],['reviewCount','review'],['approvedCount','approved'],['failedCount','failed']])$(id).textContent=items.filter(i=>i.status===status).length;$('imageCount').textContent=items.length;$('selectionCount').textContent=`${selected.size} selected`;$('footerStatus').textContent=`${items.length} images · ${items.filter(i=>i.status==='approved').length} approved`;}
function cacheGet(cache,key){if(!cache.has(key))return null;const val=cache.get(key);cache.delete(key);cache.set(key,val);return val;}
function cacheSet(cache,key,val,max){if(cache.has(key))cache.delete(key);else if(cache.size>=max)cache.delete(cache.keys().next().value);cache.set(key,val);}
async function preview(item,budget=65536,fast=true){if(demo&&item.demoUrl)return {url:item.demoUrl,width:960,height:720};return invoke('prepare_image',{path:item.path,targetPx:Math.round(budget),...(fast?{fast:true}:{})});}
function enqueuePreview(fn){
  return new Promise((resolve,reject)=>{
    const run=()=>{previewActive++;Promise.resolve().then(fn).then(resolve,reject).finally(()=>{previewActive--;if(previewPending.length)previewPending.shift()();});};
    if(previewActive<PREVIEW_CONCURRENCY)run();else previewPending.push(run);
  });
}
function enqueuePrepare(item,budget){const job=prepareQueue.then(()=>preview(item,budget,false));prepareQueue=job.catch(()=>{});return job;}
function loadThumbnail(img,item,isCurrent,fallbackAlt){
  const cached=cacheGet(thumbCache,item.path);
  if(cached){img.src=cached.url;return;}
  enqueuePreview(async()=>{
    if(!isCurrent())return;
    const hit=cacheGet(thumbCache,item.path);
    if(hit){if(isCurrent())img.src=hit.url;return;}
    try{
      const p=await preview(item,65536,true);
      cacheSet(thumbCache,item.path,p,500);
      if(isCurrent())img.src=p.url;
    }catch(e){
      if(isCurrent()){img.alt=fallbackAlt;img.title=String(e);}
    }
  });
}
function filteredImages(){const q=$('searchImages').value.toLowerCase();return items.filter(i=>i.path.toLowerCase().includes(q));}

function renderGrid(){counts();const version=++gridVersion,rows=filteredImages(),pages=Math.max(1,Math.ceil(rows.length/PAGE_SIZE));page=Math.max(0,Math.min(page,pages-1));$('imageGrid').replaceChildren();$('empty').hidden=items.length>0;$('pageLabel').textContent=`Page ${page+1} of ${pages} · ${rows.length} images`;$('prevPage').disabled=page===0;$('nextPage').disabled=page>=pages-1;
  for(const row of rows.slice(page*PAGE_SIZE,(page+1)*PAGE_SIZE)){const card=document.createElement('div');card.className='card'+(selected.has(row.path)?' selected':'');const cb=document.createElement('input');cb.type='checkbox';cb.checked=selected.has(row.path);cb.setAttribute('aria-label',`Select ${row.name}`);cb.onchange=()=>{cb.checked?selected.add(row.path):selected.delete(row.path);card.classList.toggle('selected',cb.checked);counts();};const img=document.createElement('img');img.alt=row.name;const selection=document.createElement('label');selection.className='image-select';selection.title=`Select ${row.name}`;const control=document.createElement('span');control.className='selection-control';control.append(cb);selection.append(img,control);const review=document.createElement('button');review.className='review-card';review.type='button';review.textContent='Review caption →';review.setAttribute('aria-label',`Review caption for ${row.name}`);review.onclick=()=>{currentPath=row.path;$('reviewFilter').value='all';tab('review');};const details=document.createElement('div');details.className='details';const name=document.createElement('span');name.className='name';name.textContent=row.name;name.title=row.path;const status=document.createElement('span');status.className=`badge ${row.status}`;status.textContent=row.status;details.append(name,status,review);card.append(selection,details);$('imageGrid').append(card);
  loadThumbnail(img,row,()=>version===gridVersion,'Preview unavailable');
}}

async function importImages(folder){if(running)return message('Pause the run before importing images.');$('addFiles').disabled=$('addFolder').disabled=true;try{const result=await invoke('import_images',{folder});if(!result)return;const before=items.length;items=Collection.merge(items,result.images);renderGrid();await persist();message(`Imported ${items.length-before} images.${result.warnings.length?'\n'+result.warnings.slice(0,5).join('\n'):''}`);}catch(e){error(e);}finally{$('addFiles').disabled=$('addFolder').disabled=false;}}
$('addFiles').onclick=()=>importImages(false);$('addFolder').onclick=()=>importImages(true);
$('searchImages').oninput=()=>{page=0;renderGrid();};$('prevPage').onclick=()=>{page--;renderGrid();};$('nextPage').onclick=()=>{page++;renderGrid();};$('selectPage').onclick=()=>{filteredImages().slice(page*PAGE_SIZE,(page+1)*PAGE_SIZE).forEach(i=>selected.add(i.path));renderGrid();};$('clearSelection').onclick=()=>{selected.clear();renderGrid();};$('removeSelected').onclick=()=>{if(running)return message('Pause the run before removing images.');items=items.filter(i=>!selected.has(i.path));selected.clear();renderGrid();save();};

function renderReferences(){
  const version=++referenceVersion;$('referenceGrid').replaceChildren();$('referenceCount').textContent=references.length;
  references.forEach(ref=>{
    const card=document.createElement('div');card.className='reference-card';const img=document.createElement('img');img.alt=ref.name;
    const name=document.createElement('span');name.className='name';name.textContent=ref.name;name.title=ref.path;
    const notes=document.createElement('textarea');notes.rows=3;notes.value=ref.note||'';notes.placeholder='What should the model learn from this reference?';notes.setAttribute('aria-label',`Notes for ${ref.name}`);notes.disabled=running;notes.oninput=()=>{ref.note=notes.value;save();};
    const remove=document.createElement('button');remove.textContent='Remove';remove.disabled=running;remove.onclick=()=>{references=references.filter(r=>r.path!==ref.path);for(const k of preparedRefCache.keys())if(k.startsWith(ref.path+'|'))preparedRefCache.delete(k);renderReferences();save();};
    card.append(img,name,notes,remove);$('referenceGrid').append(card);
    loadThumbnail(img,ref,()=>version===referenceVersion,'Reference unavailable');
  });
}
$('addReferences').onclick=async()=>{if(running)return;$('addReferences').disabled=true;try{const result=await invoke('import_images',{folder:false});if(!result)return;const seen=new Set(references.map(r=>r.path));for(const row of result.images){if(!seen.has(row.path)){references.push({path:row.path,name:row.name,note:row.caption||''});seen.add(row.path);}}renderReferences();await persist();if(result.warnings.length)message(result.warnings.join('\n'));}catch(e){error(e);}finally{$('addReferences').disabled=running;}};
$('clearReferences').onclick=()=>{if(running)return;references=[];preparedRefCache.clear();renderReferences();save();};

function syncCacheMode(){
  const mode=$('cacheMode').value;
  $('concurrency').disabled=running;
  $('cacheModeHelp').textContent=mode==='fork'
    ?'Keeps a named KV prefix handle (X-Halogen-Keep) before starting, and forks every target image from it (X-Halogen-From) with prompt_cache_policy: "no_store".'
    :mode==='standard'
      ?'Stores every request in halogen-flash-server’s prompt cache (default HALOGEN_PROMPT_CACHE=2). Suitable for single tests or when keeping turns in cache.'
      :'Warms the shared instruction and references once, then sends prompt_cache_policy: "no_store" on targets so finished captions do not evict the prefix.';
}
$('cacheMode').addEventListener('change',()=>{syncCacheMode();save();});

function syncThinking(){
  const eff=$('effort').value;
  $('maxThinkingTokensLabel').hidden=eff==='none'||eff==='none-qwen';
}
$('effort').addEventListener('change',()=>{syncThinking();save();});

function syncSampling(){
  const temp=Number($('temperature').value);
  $('samplingExtra').hidden=!(Number.isFinite(temp)&&temp>0);
}
$('samplingPreset').addEventListener('change',()=>{
  const preset=Collection.SAMPLING_PRESETS[$('samplingPreset').value];
  if(preset){
    $('temperature').value=preset.temperature;
    $('topP').value=preset.topP;
    $('topK').value=preset.topK;
    $('presencePenalty').value=preset.presencePenalty;
    $('repetitionPenalty').value=preset.repetitionPenalty;
  }
  syncSampling();save();
});
for(const id of ['temperature','topP','topK','presencePenalty','repetitionPenalty']){
  $(id).addEventListener('input',()=>{
    const match=Object.entries(Collection.SAMPLING_PRESETS).find(([,p])=>
      String($('temperature').value)===String(p.temperature)&&
      String($('topP').value)===String(p.topP)&&
      String($('topK').value)===String(p.topK)&&
      String($('presencePenalty').value)===String(p.presencePenalty)&&
      String($('repetitionPenalty').value)===String(p.repetitionPenalty)
    );
    $('samplingPreset').value=match?match[0]:'custom';
    syncSampling();save();
  });
}

const presetControls=['instructionPresets','instructionName','saveInstruction','loadInstruction','updateInstruction','deleteInstruction'];
let instructionPresets=[],presetsReady=false,presetsBusy=false;
function presetButtons(){
  const unavailable=running||presetsBusy||!presetsReady;
  presetControls.forEach(id=>$(id).disabled=unavailable);
  for(const id of ['loadInstruction','updateInstruction','deleteInstruction'])$(id).disabled=unavailable||!$('instructionPresets').value;
}
function renderPresets(selectedId=''){
  $('instructionPresets').replaceChildren(new Option('Choose saved instructions…',''));
  for(const preset of instructionPresets)$('instructionPresets').add(new Option(preset.name,preset.id));
  $('instructionPresets').value=selectedId;presetButtons();
}
async function loadPresets(){
  try{const stored=native?(await invoke('read_persisted')).instructions:JSON.parse(localStorage.getItem('imageproc-halogen-instructions')||'null');
    if(stored!=null&&(stored.version!==1||!Array.isArray(stored.presets)))throw Error('Saved instruction format is not recognized. Existing data has been preserved.');
    instructionPresets=(stored?.presets||[]);
    if(instructionPresets.some(p=>typeof p.id!=='string'||typeof p.name!=='string'||typeof p.text!=='string'))throw Error('Saved instructions could not be read. Existing data has been preserved.');
    presetsReady=true;renderPresets();
  }catch(e){error(e);presetButtons();}
}
async function storePresets(next,selectedId){
  presetsBusy=true;presetButtons();
  try{const data={version:1,presets:next};if(native)await invoke('write_instructions',{data});else localStorage.setItem('imageproc-halogen-instructions',JSON.stringify(data));
    instructionPresets=next;renderPresets(selectedId);message('Saved instructions updated.');
  }catch(e){error(e);}finally{presetsBusy=false;presetButtons();}
}
$('instructionPresets').onchange=presetButtons;
$('saveInstruction').onclick=()=>{
  const name=$('instructionName').value.trim(),text=$('instruction').value;
  if(!name)return message('Enter a name for these instructions.');if(!text.trim())return message('Enter caption instructions first.');
  if(instructionPresets.some(p=>p.name.toLocaleLowerCase()===name.toLocaleLowerCase()))return message('That name already exists. Select it and use Update saved, or choose another name.');
  const id=crypto.randomUUID();storePresets([...instructionPresets,{id,name,text}],id);
};
$('loadInstruction').onclick=()=>{const preset=instructionPresets.find(p=>p.id===$('instructionPresets').value);if(!preset)return;$('instruction').value=preset.text;$('instructionName').value=preset.name;save();};
$('updateInstruction').onclick=()=>{const id=$('instructionPresets').value;if(!id)return;if(!$('instruction').value.trim())return message('Enter caption instructions first.');storePresets(instructionPresets.map(p=>p.id===id?{...p,text:$('instruction').value}:p),id);};
let deletingPreset=null;
$('deleteInstruction').onclick=()=>{deletingPreset=$('instructionPresets').value;const preset=instructionPresets.find(p=>p.id===deletingPreset);if(!preset)return;$('deleteInstructionName').textContent=`Delete “${preset.name}”? The text in the editor will stay as it is.`;$('deleteInstructionDialog').showModal();};
$('keepInstruction').onclick=()=>$('deleteInstructionDialog').close();
$('confirmDeleteInstruction').onclick=()=>{$('deleteInstructionDialog').close();storePresets(instructionPresets.filter(p=>p.id!==deletingPreset),'');};
presetButtons();

$('instruction').value=Collection.anatomyPrompt;$('anatomyPreset').onclick=()=>{$('instruction').value=Collection.anatomyPrompt;save();};
fields.forEach(id=>$(id).addEventListener('input',save));
$('newProject').onclick=()=>{if(running)return message('Pause the run before starting a new collection.');$('confirmDialog').showModal();};
$('keepCollection').onclick=()=>$('confirmDialog').close();
$('confirmNew').onclick=async()=>{items=[];references=[];preparedRefCache.clear();thumbCache.clear();reviewCache.clear();renderReferences();selected.clear();currentPath=null;page=0;$('confirmDialog').close();renderGrid();await persist();tab('images');};

function connectionArgs(){const baseUrl=$('baseUrl').value.trim();let url;try{url=new URL(baseUrl);}catch{throw Error('Enter a valid server URL.');}if(!['http:','https:'].includes(url.protocol))throw Error('Server URL must use HTTP or HTTPS.');return {baseUrl,apiKey:$('apiKey').value};}

function renderServerStatus(st){
  $('halogenStatusPanel').hidden=false;
  const visionOn=st.vision_enabled!==false;
  $('visionBadge').textContent=visionOn?'Vision ready':'Vision OFF';
  $('visionBadge').className=`badge ${visionOn?'approved':'failed'}`;
  $('visionWarning').hidden=visionOn;
  if(!visionOn)$('visionWarning').textContent=st.vision_reason||'Vision tower is not loaded on this halogen-flash-server. Start the container with -e HALOGEN_VISION_TOWER=1.';
  const ver=st.version;
  $('serverVersion').textContent=ver?.engine?`${ver.engine}${ver.api&&ver.api!==ver.engine?' / API '+ver.api:''}`:'Connected';
  const pool=st.cache?.pool;
  if(pool?.positions){
    const pct=Math.round((pool.usage_ratio??((pool.used||0)/pool.positions))*100);
    $('poolSummary').textContent=`${(pool.used||0).toLocaleString()} / ${pool.positions.toLocaleString()} (${pct}%)`;
  }else{
    $('poolSummary').textContent=st.context?`${Number(st.context).toLocaleString()} ctx`:'—';
  }
  if(st.cache&&st.cache.hit_rate!=null){
    $('cacheSummary').textContent=`${Math.round(st.cache.hit_rate*100)}% req · ${Math.round((st.cache.token_hit_rate||0)*100)}% tok`;
  }else{
    $('cacheSummary').textContent='—';
  }
  $('queueSummary').textContent=`${st.slots??'?'} slots · ${st.in_flight??0} active · ${st.queued??0} queued`;
}

async function checkServer(){
  const b=$('fetchModels');b.disabled=true;$('connection').textContent='Connecting…';
  try{
    let st;
    try{st=await invoke('halogen_status',connectionArgs());}
    catch(err){
      if(!/unexpected|unknown command|not found/i.test(String(err?.message||err)))throw err;
      const rows=await invoke('list_models',connectionArgs());
      st={models:rows,model_id:rows?.[0]?.id||rows?.[0]||'',vision_enabled:true};
    }
    const rows=Array.isArray(st.models)?st.models:[];
    $('modelSelect').replaceChildren(new Option('Choose a vision model…',''));
    for(const r of rows){const id=typeof r==='string'?r:r.id;if(id)$('modelSelect').add(new Option(id,id));}
    if(!$('modelId').value.trim()&&st.model_id)$('modelId').value=st.model_id;
    $('modelSelect').value=$('modelId').value;
    $('connection').textContent=`${rows.length} model${rows.length===1?'':'s'} · ${st.vision_enabled!==false?'vision ready':'vision OFF'}`;
    renderServerStatus(st);
  }catch(e){
    $('connection').textContent='Connection failed';
    $('halogenStatusPanel').hidden=true;
    error(e);
  }finally{b.disabled=false;}
}
$('fetchModels').onclick=checkServer;
$('modelSelect').onchange=()=>{$('modelId').value=$('modelSelect').value;save();};

function optFloat(id,min,max,label){
  const raw=$(id).value.trim();
  if(!raw)return '';
  const v=Number(raw);
  if(!Number.isFinite(v)||v<min||v>max)throw Error(`${label} must be between ${min} and ${max}.`);
  return v;
}

function config(){
  const model=$('modelId').value.trim(),instruction=$('instruction').value.trim();
  if(!model)throw Error('Choose or enter a model ID in Server & model.');
  if(!instruction)throw Error('Enter caption instructions.');
  const maxTokens=Number($('maxTokens').value);
  if(!Number.isInteger(maxTokens)||maxTokens<64||maxTokens>65536)throw Error('Max completion tokens must be an integer from 64 to 65536.');
  const effort=$('effort').value;
  const rawMaxThink=$('maxThinkingTokens').value.trim();
  const maxThinkingTokens=rawMaxThink===''?0:Number(rawMaxThink);
  if(!Number.isInteger(maxThinkingTokens)||maxThinkingTokens<0||maxThinkingTokens>65536)throw Error('Max thinking tokens must be an integer from 0 to 65536.');
  const temperature=Number($('temperature').value);
  if(!Number.isFinite(temperature)||temperature<0||temperature>2)throw Error('Temperature must be between 0 and 2.');
  const topP=optFloat('topP',0,1,'Top P');
  const rawTopK=$('topK').value.trim();
  const topK=rawTopK===''?'':Number(rawTopK);
  if(topK!==''&&(!Number.isInteger(topK)||topK<0))throw Error('Top K must be a non-negative integer.');
  const presencePenalty=optFloat('presencePenalty',-2,2,'Presence penalty');
  const repetitionPenalty=optFloat('repetitionPenalty',0.1,5,'Repetition penalty');
  const budget=Number($('budget').value)*1000000;
  if(!Number.isFinite(budget)||budget<65536||budget>Collection.HALOGEN_MAX_BUDGET)throw Error('Image budget must be between 0.065536 and 14.7456 MP.');
  const concurrency=Number($('concurrency').value);
  if(!Number.isInteger(concurrency)||concurrency<1||concurrency>32)throw Error('Concurrent images must be an integer from 1 to 32.');
  const cacheMode=$('cacheMode').value;
  if(!['no_store','fork','standard'].includes(cacheMode))throw Error('Choose a Halogen prompt cache / session mode.');
  return {
    ...connectionArgs(),
    model,
    instruction,
    maxTokens,
    effort,
    maxThinkingTokens:effort==='none'||effort==='none-qwen'?0:maxThinkingTokens,
    samplingPreset:$('samplingPreset').value,
    temperature,
    topP,
    topK,
    presencePenalty,
    repetitionPenalty,
    budget,
    concurrency,
    cacheMode,
    referenceInstruction:$('referenceInstruction').value,
  };
}

function budgetKey(c){return c.baseUrl.replace(/\/+$/,'')+'|'+c.model;}

async function measureBudget(){
  const button=$('measure');button.disabled=true;
  try{
    const c=config();$('budgetHelp').textContent='Measuring with synthetic images…';
    const tokens=async content=>invoke('measure_prompt_tokens',{baseUrl:c.baseUrl,apiKey:c.apiKey,payload:{model:c.model,stream:false,max_completion_tokens:1,temperature:0,reasoning_effort:'none',chat_template_kwargs:{enable_thinking:false},messages:[{role:'user',content}]}});
    const text={type:'text',text:'Describe the image.'};
    const baseline=await tokens([text]);
    const observations=[];
    for(const side of [1024,1448,3400]){
      const canvas=document.createElement('canvas');canvas.width=canvas.height=side;
      const ctx=canvas.getContext('2d');ctx.fillStyle='#fff';ctx.fillRect(0,0,side,side);ctx.fillStyle='#222';
      for(let y=30;y<side-30;y+=24)ctx.fillRect(30,y,side-60-(y%7)*10,8);
      try{
        const n=await tokens([text,{type:'image_url',image_url:{url:canvas.toDataURL('image/png')}}]);
        observations.push({pixels:side*side,tokens:Number(n)-Number(baseline)});
      }finally{canvas.width=canvas.height=0;}
    }
    const result=Collection.estimateBudget(observations,c.budget);
    $('budget').value=(result.pixels/1000000).toFixed(4);
    $('budgetHelp').textContent=result.description;
    imageBudgets[budgetKey(c)]={pixels:result.pixels,description:result.description};
    save();return result.pixels;
  }catch(e){
    $('budgetHelp').textContent='Measurement failed; no caption run was started. Check the server before retrying. '+String(e.message||e);
    return null;
  }finally{button.disabled=running;}
}
$('measure').onclick=()=>measureBudget();
$('budget').addEventListener('change',()=>{try{const c=config();imageBudgets[budgetKey(c)]={pixels:c.budget,description:'Custom pixel budget for this server and model.'};save();}catch{}});
document.querySelectorAll('.budget-preset').forEach(btn=>{
  btn.onclick=()=>{
    $('budget').value=btn.dataset.mp;
    $('budget').dispatchEvent(new Event('change'));
  };
});

async function streamCaption(row,c,view,options={}){
  const p=await enqueuePrepare(row,c.budget);
  if(stopCurrent)throw new DOMException('Cancelled','AbortError');
  view.image.src=p.url;view.image.hidden=false;
  const id=crypto.randomUUID();
  return new Promise(async(resolve,reject)=>{
    let output='',thinking='',lastStats=null,done=false,unlisten=()=>{},timer;
    const finish=err=>{
      if(done)return;done=true;clearTimeout(timer);unlisten();cancelRequests.delete(id);
      invoke('chat_cancel',{id}).catch(()=>{});
      if(err)reject(err);
      else if(!output.trim())reject(Error('Model returned no caption.'));
      else resolve({caption:output.trim(),stats:lastStats});
    };
    const resetTimer=()=>{clearTimeout(timer);timer=setTimeout(()=>finish(Error('No response for five minutes. Check the server and retry.')),300000);};
    try{
      unlisten=await window.__TAURI__.event.listen('chat-stream',e=>{
        const ev=e.payload;if(ev.id!==id||done)return;resetTimer();
        if(ev.kind==='progress'&&ev.progress){
          const pp=ev.progress;
          view.meta.textContent=`Prefilling prompt: ${pp.processed??0} / ${pp.total??'?'} tokens (${pp.cache??0} cached)`;
        }else if(ev.kind==='output'){
          output+=ev.text||'';view.caption.textContent=output;
          if(Repetition.detect(output))finish(Error('Repetition detected; caption stopped.'));
        }else if(ev.kind==='thinking'){
          thinking+=ev.text||'';
          if(!output&&!lastStats)view.meta.textContent=`Thinking… (${thinking.length} chars)`;
          if(Repetition.detect(thinking))finish(Error('Repetition in model reasoning; caption stopped.'));
        }else if(ev.kind==='stats'){
          lastStats={stats:ev.stats||null,usage:ev.usage||null,halogenFrom:ev.halogen_from||null};
          const summary=Collection.formatStats(lastStats.stats,lastStats.usage,lastStats.halogenFrom);
          if(summary)view.meta.textContent=summary;
        }else if(ev.kind==='done')finish();
        else if(ev.kind==='error')finish(Error(ev.message||'Generation failed'));
      });
      cancelRequests.set(id,()=>finish(new DOMException('Cancelled','AbortError')));
      if(stopCurrent){finish(new DOMException('Cancelled','AbortError'));return;}
      resetTimer();
      await invoke('chat_stream',{
        id,
        baseUrl:c.baseUrl,
        apiKey:c.apiKey,
        headers:options.headers||null,
        payload:Collection.buildChatPayload(c,p.url,{noStore:!!options.noStore}),
      });
    }catch(e){finish(e);}
  });
}

function lockRun(value){
  running=value;
  for(const id of [...fields,'modelSelect','fetchModels','measure','anatomyPreset','addReferences','clearReferences','start','addFiles','addFolder','removeSelected','regenerateSelected','newProject','regenerate','export','exportMode','exportScope'])$(id).disabled=value;
  document.querySelectorAll('.budget-preset').forEach(el=>el.disabled=value);
  $('pause').disabled=$('cancel').disabled=!value;
  presetButtons();syncReviewControls();syncCacheMode();
  document.querySelectorAll('#referenceGrid textarea,#referenceGrid button').forEach(el=>el.disabled=value);
}

function liveView(slot){
  const container=document.createElement('div');container.className='live';
  const image=document.createElement('img');image.alt='Current image';image.hidden=true;
  const body=document.createElement('div'),name=document.createElement('h3'),meta=document.createElement('p'),caption=document.createElement('pre');
  meta.className='live-meta';
  name.textContent=`Worker ${slot+1}`;body.append(name,meta,caption);container.append(image,body);$('liveStreams').append(container);
  return {image,name,meta,caption};
}
function cancelAll(){for(const cancel of [...cancelRequests.values()])cancel();}

async function runQueue(rows){
  if(running)return;let c;try{c=config();}catch(e){error(e);tab('model');return;}
  if(!rows.length)return message('No images match this queue.');if(!native)return message('Captioning runs in the desktop app.');
  queuedPaths=new Set(rows.map(r=>r.path));lockRun(true);pauseRequested=false;stopCurrent=false;tab('run');$('progress').max=rows.length;$('progress').value=0;$('liveStreams').replaceChildren();$('runTelemetry').hidden=true;$('runTelemetry').textContent='';
  let completed=0,next=0,activeHandle=null,prefixWarmed=false;
  const runStats={count:0,promptTokens:0,cachedTokens:0,tpsSum:0,tpsCount:0,draftN:0,draftAcc:0};
  const recordTelemetry=entry=>{
    runStats.count++;
    const s=entry?.stats,u=entry?.usage;
    const cacheN=Number(s?.cache_n??u?.prompt_tokens_details?.cached_tokens??0);
    const promptN=Number(s?.prompt_n??0);
    const totalPrompt=Number.isFinite(Number(u?.prompt_tokens))?Number(u.prompt_tokens):(promptN+cacheN);
    if(totalPrompt>0){runStats.promptTokens+=totalPrompt;runStats.cachedTokens+=Math.min(totalPrompt,Math.max(0,cacheN));}
    if(s?.predicted_per_second!=null&&Number.isFinite(Number(s.predicted_per_second))){runStats.tpsSum+=Number(s.predicted_per_second);runStats.tpsCount++;}
    if(s?.draft_n>0&&s?.draft_n_accepted!=null&&s.draft_n_accepted<=s.draft_n){runStats.draftN+=Number(s.draft_n);runStats.draftAcc+=Number(s.draft_n_accepted);}
    const parts=[`Completed ${runStats.count}`];
    if(runStats.promptTokens>0)parts.push(`Cache hit ${Math.round(100*runStats.cachedTokens/runStats.promptTokens)}% (${runStats.cachedTokens.toLocaleString()}/${runStats.promptTokens.toLocaleString()} tok)`);
    if(runStats.tpsCount>0)parts.push(`Mean decode ${(runStats.tpsSum/runStats.tpsCount).toFixed(1)} tok/s`);
    if(runStats.draftN>0)parts.push(`MTP accept ${Math.round(100*runStats.draftAcc/runStats.draftN)}% (${runStats.draftAcc}/${runStats.draftN})`);
    $('runTelemetry').textContent=parts.join(' · ');$('runTelemetry').hidden=false;
  };
  const progress=()=>{$('progress').value=completed;$('runStatus').textContent=`${completed} / ${rows.length} completed · ${rows.filter(r=>r.status==='running').length} active`;counts();refreshReviewProgress();};
  try{
    await invoke('inhibit_suspend',{active:true});
    const saved=imageBudgets[budgetKey(c)];if(saved){c.budget=saved.pixels;$('budget').value=(c.budget/1000000).toFixed(4);$('budgetHelp').textContent=saved.description;}else{$('budgetHelp').textContent='Using the configured image pixel budget. Measurement runs only when requested.';}
    c.references=[];
    for(const ref of references){
      if(pauseRequested)break;
      const refKey=`${ref.path}|${Math.round(c.budget)}`;
      let prepared=preparedRefCache.get(refKey);
      if(!prepared){
        $('runStatus').textContent=`Preparing reference: ${ref.name}`;
        prepared=await enqueuePrepare(ref,c.budget);
        preparedRefCache.set(refKey,prepared);
      }
      c.references.push({url:prepared.url,note:ref.note||''});
    }
    if(!pauseRequested){
      const warmPayload={model:c.model,stream:false,max_completion_tokens:1,temperature:0,reasoning_effort:'none',chat_template_kwargs:{enable_thinking:false},messages:Collection.prefixMessages(c)};
      if(c.cacheMode==='fork'){
        activeHandle='imageproc-'+crypto.randomUUID().slice(0,8);
        $('runStatus').textContent=`Keeping Halogen session handle (${activeHandle})…`;
        await invoke('halogen_warm_prefix',{baseUrl:c.baseUrl,apiKey:c.apiKey,handleName:activeHandle,payload:warmPayload});
        prefixWarmed=true;
      }else if(c.cacheMode==='no_store'&&rows.length>1&&(c.concurrency>1||c.references.length>0)){
        $('runStatus').textContent='Warming shared Halogen prefix…';
        try{
          await invoke('halogen_warm_prefix',{baseUrl:c.baseUrl,apiKey:c.apiKey,handleName:null,payload:warmPayload});
          prefixWarmed=true;
        }catch(e){
          if(Collection.serverUnavailable(String(e?.message||e)))throw e;
          prefixWarmed=false;
        }
      }
    }
    const worker=async slot=>{
      const view=liveView(slot);
      try{while(!pauseRequested&&next<rows.length){
        const idx=next++;const row=rows[idx];
        row.status='running';row.error='';view.name.textContent=row.name;view.meta.textContent='Preparing image…';view.caption.textContent='';view.image.hidden=true;progress();await persist();
        const useNoStore=c.cacheMode==='fork'||(c.cacheMode==='no_store'&&(prefixWarmed||rows.length===1||idx>0));
        const reqHeaders=activeHandle?{'X-Halogen-From':activeHandle}:null;
        try{
          const res=await streamCaption(row,c,view,{noStore:useNoStore,headers:reqHeaders});
          row.caption=res.caption;row.status='review';
          const statsText=res.stats?Collection.formatStats(res.stats.stats,res.stats.usage,res.stats.halogenFrom):'';
          row.provenance={
            model:c.model,
            instruction:c.instruction,
            at:new Date().toISOString(),
            references:references.map(r=>({path:r.path,note:r.note})),
            cacheMode:c.cacheMode,
            samplingPreset:c.samplingPreset,
            timings:res.stats?.stats||null,
            usage:res.stats?.usage||null,
            halogenFrom:res.stats?.halogenFrom||null,
            statsText,
          };
          if(res.stats)recordTelemetry(res.stats);
        }catch(e){
          row.status=e.name==='AbortError'?'pending':'failed';
          row.error=String(e.message||e);
          view.meta.textContent='Failed';
          view.caption.textContent=row.error;
          if(Collection.serverUnavailable(row.error)){
            pauseRequested=true;stopCurrent=true;cancelAll();
            message('The halogen-flash-server connection or state failed. The queue is paused; check the server before retrying.');
          }
        }
        queuedPaths.delete(row.path);completed++;progress();await persist();
      }}catch(e){pauseRequested=true;error(e);}
    };
    if(!pauseRequested){
      await Promise.all(Array.from({length:Math.min(c.concurrency,rows.length)},(_,i)=>worker(i)));
    }
  }catch(e){pauseRequested=true;error(e);}finally{
    if(activeHandle){
      await invoke('halogen_release_handle',{baseUrl:c.baseUrl,apiKey:c.apiKey,handleName:activeHandle}).catch(()=>{});
    }
    await invoke('inhibit_suspend',{active:false}).catch(error);
    items.filter(i=>i.status==='running').forEach(i=>i.status='pending');queuedPaths.clear();lockRun(false);refreshReviewProgress();
    $('runStatus').textContent=`${pauseRequested?'Paused':'Finished'} · ${completed} attempted · ${items.filter(i=>i.status==='review').length} ready for review`;counts();persist().catch(error);
  }
}

$('regenerateSelected').onclick=()=>{if(running)return;$('runScope').value='regenerate-selected';runQueue(Collection.eligible(items,'regenerate-selected',selected));};
$('start').onclick=()=>runQueue(Collection.eligible(items,$('runScope').value,selected));
$('pause').onclick=()=>{pauseRequested=true;$('runStatus').textContent='Will pause after all active images finish.';};
$('cancel').onclick=()=>{pauseRequested=true;stopCurrent=true;cancelAll();$('runStatus').textContent='Stopping active images…';};

function reviewRows(){const f=$('reviewFilter').value;return items.filter(i=>f==='all'||i.status===f);}
function activeItem(){return items.find(i=>i.path===currentPath);}
function reviewLocked(row){return !!row&&(row.status==='running'||queuedPaths.has(row.path));}
function syncReviewControls(){const row=activeItem(),locked=!row||reviewLocked(row);for(const id of ['caption','approve','reject'])$(id).disabled=locked;$('regenerate').disabled=running||!row;}
function refreshReviewProgress(){
  syncReviewControls();if(!$('review').classList.contains('active'))return;
  const row=activeItem();
  if(!row||(reviewShownLocked&&!reviewLocked(row))){renderReview();return;}
  const rows=reviewRows(),index=rows.findIndex(i=>i.path===currentPath);
  $('reviewPosition').textContent=`${index>=0?index+1:0} / ${rows.length}`;
  $('reviewPrev').disabled=index<=0;$('reviewNext').disabled=index>=rows.length-1;
}
async function renderReview(){counts();const version=++reviewVersion,rows=reviewRows();let index=rows.findIndex(i=>i.path===currentPath);if(index<0)index=0;const row=rows[index];currentPath=row?.path||null;reviewShownLocked=reviewLocked(row);syncReviewControls();$('reviewEmpty').hidden=!!row;$('reviewWorkspace').hidden=!row;$('reviewPosition').textContent=`${row?index+1:0} / ${rows.length}`;$('reviewPrev').disabled=index<=0;$('reviewNext').disabled=index>=rows.length-1;if(!row)return;$('reviewName').textContent=row.name;$('reviewPath').textContent=row.path;$('reviewStatus').textContent=row.status;$('reviewStatus').className=`badge ${row.status}`;$('caption').value=row.caption;$('captionMeta').textContent=`${row.caption.length} characters${row.provenance?' · '+row.provenance.model:''}${row.provenance?.statsText?' · '+row.provenance.statsText:''}`;$('reviewError').textContent=row.error||'';const cachedReview=cacheGet(reviewCache,row.path);if(cachedReview){$('reviewImage').src=cachedReview.url;$('dimensions').textContent=`${cachedReview.originalWidth||cachedReview.width} × ${cachedReview.originalHeight||cachedReview.height} original pixels`;return;}const cachedThumb=cacheGet(thumbCache,row.path);if(cachedThumb)$('reviewImage').src=cachedThumb.url;else $('reviewImage').removeAttribute('src');$('dimensions').textContent='Loading preview…';try{const p=await preview(row,900000,true);cacheSet(reviewCache,row.path,p,24);if(version!==reviewVersion)return;$('reviewImage').src=p.url;$('dimensions').textContent=`${p.originalWidth||p.width} × ${p.originalHeight||p.height} original pixels`;}catch(e){if(version===reviewVersion)$('dimensions').textContent=String(e);}}
function moveReview(delta){const rows=reviewRows(),idx=rows.findIndex(i=>i.path===currentPath);currentPath=rows[Math.max(0,Math.min(rows.length-1,idx+delta))]?.path;renderReview();}
$('reviewFilter').onchange=()=>{currentPath=null;renderReview();};$('reviewPrev').onclick=()=>moveReview(-1);$('reviewNext').onclick=()=>moveReview(1);
$('caption').oninput=()=>{const row=activeItem();if(!row||reviewLocked(row))return;row.caption=$('caption').value;row.status='review';row.error='';$('reviewStatus').textContent='review';$('reviewStatus').className='badge';$('captionMeta').textContent=`${row.caption.length} characters`;counts();save();};
function disposition(status){const row=activeItem();if(!row||reviewLocked(row))return;if(status==='approved'&&!row.caption.trim())return message('Write a caption before approving.');const rows=reviewRows(),index=rows.findIndex(i=>i.path===row.path);row.status=status;currentPath=rows[index+1]?.path||rows[index-1]?.path||null;save();renderReview();}
$('approve').onclick=()=>disposition('approved');$('reject').onclick=()=>disposition('rejected');document.addEventListener('keydown',e=>{if(e.ctrlKey&&e.key==='Enter'&&$('review').classList.contains('active')){e.preventDefault();disposition('approved');}});
$('regenerate').onclick=()=>{const row=activeItem();if(row)runQueue([row]);};
$('export').onclick=async()=>{const rows=Collection.exportable(items,$('exportScope').value);if(!rows.length)return message('No captions match this export. Approve some images first.');$('export').disabled=true;try{await persist();const result=await invoke('export_dataset',{entries:rows.map(({path,caption,status})=>({path,caption,status})),copyImages:true,updateExisting:$('exportMode').value==='update'});if(result)message(`Exported ${result.count} image–text pairs:\n${result.path}`);}catch(e){error(e);}finally{$('export').disabled=false;}};

async function boot(){
  await loadPresets();
  try{
    const data=native?await invoke('read_recovery'):JSON.parse(localStorage.getItem('imageproc-halogen-preview')||'null');
    if(data?.version===1){
      items=Collection.restore(data.items||[]);
      imageBudgets=data.imageBudgets||{};
      references=Array.isArray(data.references)?data.references:[];
      applySettings(data.settings);
      if(items.length)message(`Restored ${items.length} images and saved captions.`);
    }
  }catch(e){error(e);}
  if(demo){
    items=Array.from({length:8},(_,i)=>({name:`anatomy-study-${String(i+1).padStart(3,'0')}.png`,path:`/demo/anatomy-study-${i+1}.png`,caption:i<5?'A close-up study of a hand and forearm, seen from the side in soft natural light.':'',status:i<2?'approved':i<5?'review':'pending',demoUrl:`data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="960" height="720"><rect width="960" height="720" fill="${['#354951','#475455','#4b4148'][i%3]}"/><circle cx="480" cy="310" r="150" fill="#8ca59e" opacity=".25"/><text x="480" y="330" fill="#d9e6e0" text-anchor="middle" font-family="sans-serif" font-size="32">Sample image ${i+1}</text><text x="480" y="380" fill="#adc5bd" text-anchor="middle" font-family="sans-serif" font-size="20">Preview data · not a training image</text></svg>`)}`}));
    $('footerStatus').textContent='DEMO PREVIEW';
  }
  $('saveState').textContent=native?'Saved locally':'Browser preview';
  renderGrid();renderReferences();syncThinking();syncSampling();syncCacheMode();
}
if(native)window.__TAURI__.window.getCurrentWindow().onCloseRequested(async event=>{event.preventDefault();pauseRequested=true;stopCurrent=true;cancelAll();while(running)await new Promise(r=>setTimeout(r,100));try{await persist();await window.__TAURI__.window.getCurrentWindow().destroy();}catch(e){error(e);}});
window.addEventListener('error',e=>error(e.message));window.addEventListener('unhandledrejection',e=>error(e.reason));boot();
