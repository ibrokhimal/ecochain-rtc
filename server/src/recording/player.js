'use strict';
(() => {
  const $ = id => document.getElementById(id);
  const api = location.pathname.replace(/\/player$/, '');
  const recordingId = new URLSearchParams(location.search).get('recording');
  let handoff = new URLSearchParams(location.hash.slice(1)).get('handoff');
  history.replaceState(null, '', location.pathname + location.search);
  const clock = $('clock'), stage = $('stage'), context = stage.getContext('2d');
  const videos = new Map(), images = new Map(), tiles = new Map();
  let subtitles = [];
  let job, events = [], state = {}, eventIndex = -1, lastPosition = -1, pinned = null;
  const fileUrl = id => `${api}/files/${encodeURIComponent(id)}`;
  const timeLabel = ms => { const n = Math.floor(ms / 1000); return `${Math.floor(n / 60).toString().padStart(2, '0')}:${(n % 60).toString().padStart(2, '0')}`; };
  async function get(url, options = {}) {
    const response = await fetch(url, {credentials:'same-origin', ...options});
    if (!response.ok) throw new Error(`So‘rov bajarilmadi (${response.status}). Ilovadan yozuvni qayta oching.`);
    return response;
  }
  function fail(error) { $('message').textContent = error.message || 'Yozuvni ochib bo‘lmadi.'; $('message').hidden = false; $('status').textContent = 'Yozuv ochilmadi'; $('status').classList.add('error'); }
  function apply(change) {
    if (change.instance_id !== state.instance_id || change.revision !== state.revision + 1) throw new Error('Yozuvdagi chizmalar tarixi to‘liq emas.');
    for (const [key, value] of Object.entries(change.patch.set || {})) state[key] = value;
    for (const [key, value] of Object.entries(change.patch.collections || {})) {
      const idKey = key === 'hands' ? 'participant_id' : 'id';
      const items = new Map((state[key] || []).map(item => [item[idKey], item]));
      for (const id of value.remove_ids || []) items.delete(id);
      for (const item of value.upsert || []) items.set(item[idKey], {...(items.get(item[idKey]) || {}), ...item});
      state[key] = [...items.values()];
    }
    state.revision = change.revision;
  }
  function seekState(ms) {
    if (ms < lastPosition || eventIndex < 0) { eventIndex = -1; state = {}; }
    while (eventIndex + 1 < events.length && events[eventIndex + 1].time_ms <= ms) {
      const row = events[++eventIndex];
      if (row.state) state = JSON.parse(JSON.stringify(row.state)); else if (row.change) apply(row.change);
    }
    lastPosition = ms;
  }
  function resize(canvas) {
    const ratio = Math.min(devicePixelRatio || 1, 2), rect = canvas.getBoundingClientRect();
    const w = Math.max(1, Math.round(rect.width * ratio)), h = Math.max(1, Math.round(rect.height * ratio));
    if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
  }
  function activeTracks(ms) {
    const latest = new Map();
    for (const track of job.tracks || []) if (ms >= track.start_ms && ms < track.end_ms) {
      const key = `${track.participant_id}/${track.source}`;
      const prior = latest.get(key);
      if (!prior || track.start_ms >= prior.start_ms) latest.set(key, track);
    }
    return [...latest.values()];
  }
  function videoFor(track, ms) {
    let video = videos.get(track.file_id);
    if (!video) {
      video = document.createElement('video'); video.preload = 'auto'; video.muted = true; video.playsInline = true;
      video.addEventListener('loadeddata', () => { if (clock.paused) draw(); });
      video.addEventListener('seeked', () => { if (clock.paused) draw(); });
      video.src = fileUrl(track.file_id); $('media').append(video); videos.set(track.file_id, video);
    }
    const target = Math.max(0, (ms - track.start_ms) / 1000);
    if (video.readyState >= 1 && Math.abs(video.currentTime - target) > (clock.paused ? .08 : .35)) {
      try { video.currentTime = target; } catch (_) { /* metadata arrives asynchronously */ }
    }
    if (clock.paused) video.pause(); else if (video.paused) video.play().catch(() => {});
    return video;
  }
  function fit(areaW, areaH, width, height) { const scale = Math.min(areaW / width, areaH / height); return {x:(areaW-width*scale)/2,y:(areaH-height*scale)/2,w:width*scale,h:height*scale}; }
  function frame(ctx, canvas, track, video) {
    const rotated = track.rotation === 90 || track.rotation === 270;
    const width = video.videoWidth || 1920, height = video.videoHeight || 1080;
    const rect = fit(canvas.width, canvas.height, rotated ? height : width, rotated ? width : height);
    if (video.readyState >= 2) {
      const scale = Math.min(rect.w / (rotated ? height : width), rect.h / (rotated ? width : height));
      ctx.save(); ctx.translate(canvas.width/2,canvas.height/2); ctx.rotate((track.rotation || 0) * Math.PI/180);
      ctx.drawImage(video,-width*scale/2,-height*scale/2,width*scale,height*scale); ctx.restore();
    }
    return rect;
  }
  function color(value) { return `#${(Number(value) >>> 0).toString(16).padStart(8,'0').slice(2)}`; }
  function imageFor(id) {
    if (!images.has(id)) { const image = new Image(); image.src = fileUrl(id); images.set(id,image); }
    return images.get(id);
  }
  function annotations(ctx, rect) {
    const presentation = state.presentation;
    if (!presentation || presentation.paused) return;
    for (const object of state.objects || []) {
      if (object.deleted || object.presentation_id !== presentation.id || (object.page || 0) !== (presentation.page || 0)) continue;
      const points = (object.points || []).map(p => ({x:rect.x+p[0]*rect.w,y:rect.y+p[1]*rect.h}));
      if (!points.length) continue;
      const first = points[0], last = points[points.length-1];
      const x=Math.min(first.x,last.x),y=Math.min(first.y,last.y),w=Math.abs(last.x-first.x),h=Math.abs(last.y-first.y);
      ctx.save(); ctx.strokeStyle=color(object.color); ctx.fillStyle=ctx.strokeStyle; ctx.lineWidth=(object.width||.004)*rect.w; ctx.lineCap='round'; ctx.lineJoin='round';
      if (object.tool==='highlighter') ctx.globalAlpha=.35;
      switch(object.tool) {
        case 'rectangle': ctx.strokeRect(x,y,w,h); break;
        case 'ellipse': ctx.beginPath();ctx.ellipse(x+w/2,y+h/2,w/2,h/2,0,0,2*Math.PI);ctx.stroke();break;
        case 'arrow': {
          const angle=Math.atan2(last.y-first.y,last.x-first.x),length=Math.max(12,ctx.lineWidth*4);
          ctx.beginPath();ctx.moveTo(first.x,first.y);ctx.lineTo(last.x,last.y);
          for(const a of [angle-.5,angle+.5]){ctx.moveTo(last.x,last.y);ctx.lineTo(last.x-Math.cos(a)*length,last.y-Math.sin(a)*length);}ctx.stroke();break;
        }
        case 'image': {
          const image=imageFor(object.file_id);
          if(image.complete&&image.naturalWidth){const target=fit(w,h,image.naturalWidth,image.naturalHeight);ctx.drawImage(image,x+target.x,y+target.y,target.w,target.h);}break;
        }
        case 'text': case 'sticky': {
          const font=Math.max(14*Math.min(devicePixelRatio||1,2),rect.w*.018);ctx.font=`${font}px -apple-system,BlinkMacSystemFont,Segoe UI,sans-serif`;ctx.textBaseline='top';
          const maxWidth=Math.max(font,rect.x+rect.w-first.x),lines=[];
          for(const paragraph of String(object.text||'').split('\n')){let line='';for(const word of paragraph.split(' ')){if(ctx.measureText(`${line} ${word}`).width>maxWidth&&line){lines.push(line);line=word;}else{line+=(line?' ':'')+word;}}lines.push(line);}
          if(object.tool==='sticky'){const width=Math.max(...lines.map(line=>ctx.measureText(line).width),1);ctx.fillRect(first.x-5,first.y-5,width+10,lines.length*font*1.2+10);ctx.fillStyle='#182127';}
          lines.forEach((line,i)=>ctx.fillText(line,first.x,first.y+i*font*1.2));break;
        }
        default: ctx.beginPath();ctx.moveTo(first.x,first.y);if(points.length===1){ctx.arc(first.x,first.y,ctx.lineWidth/2,0,Math.PI*2);ctx.fill();}else{for(const p of points.slice(1))ctx.lineTo(p.x,p.y);ctx.stroke();}
      }
      ctx.restore();
    }
  }
  function draw() {
    if (!job) return;
    try {
      const ms=Math.min(clock.currentTime*1000,job.duration_ms);seekState(ms);resize(stage);
      const lines=subtitles.filter(row=>ms>=row.start_ms&&ms<row.end_ms).slice(-3).map(row=>`${row.name}: ${row.text}`);
      $('subtitles').textContent=lines.join('\n');$('subtitles').hidden=!lines.length;
      context.fillStyle='#070b0e';context.fillRect(0,0,stage.width,stage.height);
      const active=activeTracks(ms),presentation=state.presentation;
      let primary=presentation?.kind==='screen'?active.find(t=>t.source==='screen'&&t.participant_id===presentation.presenter_id):null;
      if(!presentation)primary=active.find(t=>t.source==='screen')||active.find(t=>t.participant_id===pinned&&t.source==='camera')||active.find(t=>t.source==='camera');
      const wanted=new Set();let rect=fit(stage.width,stage.height,1920,1080);
      if(presentation?.paused){$('message').textContent='Taqdimot vaqtincha to‘xtatilgan';$('message').hidden=false;}
      else if(presentation?.kind==='board'){context.fillStyle='#f9faf7';context.fillRect(0,0,stage.width,stage.height);$('message').hidden=true;annotations(context,rect);}
      else if(primary){const video=videoFor(primary,ms);wanted.add(primary.file_id);rect=frame(context,stage,primary,video);$('message').hidden=video.readyState>=2;if(!$('message').hidden)$('message').textContent='Video yuklanmoqda…';annotations(context,rect);}
      else{$('message').hidden=false;$('message').textContent=presentation?.kind==='screen'?'Taqdimot tasviri kutilmoqda':'Uchrashuvning audio yozuvi';}
      const cameras=active.filter(t=>t.source==='camera'&&t.file_id!==primary?.file_id).slice(0,6),needed=new Set(cameras.map(t=>t.participant_id));
      for(const [id,tile]of tiles)if(!needed.has(id)){tile.figure.remove();tiles.delete(id);}
      for(const track of cameras){
        let tile=tiles.get(track.participant_id);
        if(!tile){const figure=document.createElement('figure'),canvas=document.createElement('canvas'),caption=document.createElement('figcaption');figure.className='person';caption.textContent=track.name||'Ishtirokchi';figure.append(canvas,caption);figure.addEventListener('click',()=>{pinned=track.participant_id;});$('people').append(figure);tile={figure,canvas};tiles.set(track.participant_id,tile);}
        resize(tile.canvas);const ctx=tile.canvas.getContext('2d');ctx.fillStyle='#121c21';ctx.fillRect(0,0,tile.canvas.width,tile.canvas.height);const video=videoFor(track,ms);wanted.add(track.file_id);frame(ctx,tile.canvas,track,video);
      }
      for(const [id,video]of videos)if(!wanted.has(id)){video.pause();video.removeAttribute('src');video.load();video.remove();videos.delete(id);}
      $('position').textContent=`${timeLabel(ms)} / ${timeLabel(job.duration_ms)}`;
    }catch(error){clock.pause();fail(error);}
  }
  async function load() {
    if(!/^[a-f0-9-]{36}$/i.test(recordingId||''))throw new Error('Yozuv manzili noto‘g‘ri.');
    if(handoff){await get(`${api}/playback-session`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({handoff})});handoff=null;}
    const archive=await(await get(api)).json();job=(archive.recordings||[]).find(item=>item.id===recordingId);
    if(!job)throw new Error('Yozuv topilmadi.');
    const transcriptJobs=(archive.jobs||[]).filter(row=>row.kind==='transcript'&&row.status==='ready');
    const recorded=transcriptJobs.find(row=>row.recording_id===job.id);
    const selected=recorded?[recorded]:transcriptJobs.filter(row=>row.mode==='live'&&Number.isFinite(row.started_at_ms)&&Number.isFinite(job.started_at_ms));
    subtitles=selected.flatMap(row=>(row.segments||[]).map(segment=>({...segment,start_ms:segment.start_ms+(row.started_at_ms-job.started_at_ms||0),end_ms:segment.end_ms+(row.started_at_ms-job.started_at_ms||0)})));
    if(job.status==='failed')throw new Error('Yozuvni tayyorlashda xatolik yuz bergan.');
    if(job.status!=='ready'){$('message').textContent='Yozuv hali tayyorlanmoqda. Bu sahifa avtomatik yangilanadi.';setTimeout(()=>load().catch(fail),3000);return;}
    const response=await get(fileUrl(job.events_file_id));if(Number(response.headers.get('content-length'))>70*1024*1024)throw new Error('Chizmalar tarixi juda katta.');
    events=(await response.text()).split('\n').filter(line=>line.trim()).map(line=>JSON.parse(line));
    if(!events.length||!events[0].state)throw new Error('Yozuvning boshlang‘ich holati topilmadi.');
    for(const item of [{file_id:job.audio_file_id,label:'Umumiy audio',extension:'m4a'},...(job.tracks||[]).map((track,index)=>({file_id:track.file_id,label:`${track.name||'Ishtirokchi'} · ${track.source==='screen'?'ekran':'kamera'} · ${timeLabel(track.start_ms)}`,extension:`${index}.mp4`}))]){
      const link=document.createElement('a');link.href=fileUrl(item.file_id);link.download=`meeting-${item.extension}`;link.textContent=item.label;$('downloads').append(link);
    }
    clock.src=fileUrl(job.audio_file_id);$('status').textContent='Uchrashuv yozuvi';$('message').textContent='Ijroni boshlash uchun Play tugmasini bosing.';
    clock.addEventListener('timeupdate',draw);clock.addEventListener('seeking',draw);clock.addEventListener('pause',draw);clock.addEventListener('loadedmetadata',draw);
    clock.addEventListener('play',()=>{for(const video of videos.values())video.play().catch(()=>{});});
    const loop=()=>{if(!clock.paused)draw();requestAnimationFrame(loop);};requestAnimationFrame(loop);draw();
  }
  load().catch(fail);
})();
