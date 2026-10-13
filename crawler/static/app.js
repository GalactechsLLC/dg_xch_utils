'use strict';
const el = id => document.getElementById(id);
const number = value => Number(value).toLocaleString();
const date = seconds => seconds == null ? '—' : new Date(seconds * 1000).toLocaleString();
const ms = value => value == null ? '—' : `${value.toFixed(1)} ms`;
let pageIndex = 0, cursors = [null], nextCursor = null, total = 0, series = [], busy = false, peerGeneration = 0, latestVersions = null;
const limit = 50;
let autoReload = true;
const phaseName = phase => ({tcp_connect:"TCP connection",tls:"TLS",websocket:"WebSocket",handshake:"Chia handshake",peer_request:"Peer request",unknown:"Probe"}[phase] || "Probe");
async function get(path) {
  const response = await fetch(path, {cache: 'no-store', signal: AbortSignal.timeout(10000)});
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
  return response.json();
}
function text(id, value) { el(id).textContent = value; }
function chart() {
  const canvas = el('history'), ratio = window.devicePixelRatio || 1;
  const width = canvas.clientWidth, height = 240;
  canvas.width = width * ratio; canvas.height = height * ratio;
  const ctx = canvas.getContext('2d'); ctx.scale(ratio, ratio);
  const theme = getComputedStyle(document.body);
  const color = name => theme.getPropertyValue(name).trim();
  ctx.font = '11px Sora, sans-serif'; ctx.fillStyle = color('--muted');
  if (series.length < 2) { ctx.fillText('Collecting observations. The next sample arrives within a minute.', 12, 110); return; }
  const left = 48, right = width - 12, top = 14, bottom = height - 28;
  const max = Math.max(1, ...series.map(p => p.known));
  for (let i=0; i<=4; i++) {
    const y = bottom - (bottom-top) * i/4;
    ctx.strokeStyle=color('--line'); ctx.beginPath(); ctx.moveTo(left,y); ctx.lineTo(right,y); ctx.stroke();
    ctx.fillStyle=color('--muted'); ctx.fillText(number(Math.round(max*i/4)), 0, y+4);
  }
  const first = series[0].timestamp, last = series[series.length-1].timestamp;
  for (const [key,stroke] of [['known',color('--accent')],['recent',color('--chart-blue')],['identities',color('--chart-amber')]]) {
    ctx.strokeStyle=stroke; ctx.lineWidth=2; ctx.beginPath();
    series.forEach((p,i) => { const x=left+(right-left)*(p.timestamp-first)/(last-first); const y=bottom-(bottom-top)*p[key]/max; if(i===0)ctx.moveTo(x,y); else ctx.lineTo(x,y); });
    ctx.stroke();
  }
  ctx.fillStyle=color('--muted'); ctx.fillText(new Date(first*1000).toLocaleTimeString(),left,height-5);
  ctx.textAlign='right'; ctx.fillText(new Date(last*1000).toLocaleTimeString(),right,height-5);
}
function versions(counts) {
  const root = el('versions'); root.replaceChildren();
  const entries = Object.entries(counts).sort((a,b)=>b[1]-a[1]);
  const sum = entries.reduce((n,p)=>n+p[1],0);
  if (!sum) { root.textContent='Waiting for successful connections.'; return; }
  const visible = entries.slice(0,8), other = entries.slice(8).reduce((n,p)=>n+p[1],0);
  if (other) visible.push(['Other reported versions',other]);
  visible.forEach(([version,count])=>{
    const row=document.createElement('div'); row.className='version';
    const label=document.createElement('div'); label.className='version-label';
    const name=document.createElement('span'); name.textContent=version || 'Unspecified';
    const value=document.createElement('span'); value.textContent=`${number(count)} · ${(count/sum*100).toFixed(1)}%`;
    label.append(name,value);
    const track=document.createElement('div'); track.className='track';
    const fill=document.createElement('canvas'); fill.className='fill'; fill.width=300; fill.height=6;
    const context=fill.getContext('2d'); context.fillStyle=getComputedStyle(document.body).getPropertyValue('--accent').trim(); context.fillRect(0,0,300*count/sum,6);
    track.append(fill); row.append(label,track); root.append(row);
  });
}
async function peers() {
  const generation = ++peerGeneration;
  const search=el('search').value;
  const query = new URLSearchParams({limit, search, sort:el('sort').value, order:el('order').value, status:el('result-filter').value, family:el('family-filter').value});
  if(cursors[pageIndex]) query.set('cursor',cursors[pageIndex]);
  const data=await get(`/api/peers?${query}`);
  if(generation!==peerGeneration) return;
  total=data.total;
  nextCursor=data.next_cursor;
  if(!data.peers.length && pageIndex>0){pageIndex--;cursors.length=pageIndex+1;return peers();}
  const body=el('peers');
  // Capture the current UI after fetching, so a click made during the request is preserved.
  const open = new Set(Array.from(body.querySelectorAll('tr')).filter(row=>row.querySelector('details[open]')).map(row=>row.dataset.endpoint));
  const focused = document.activeElement?.closest('#peers tr')?.dataset.endpoint;
  const scroll = body.closest('.table-scroll'); const scrollTop = scroll.scrollTop;
  body.replaceChildren();
  data.peers.forEach(p=>{
    const tr=document.createElement('tr'); tr.dataset.endpoint=p.endpoint;
    for(const value of [p.endpoint,p.software_version || '—',date(p.first_seen),date(p.last_success),ms(p.peer_request_ms)]){
      const td=document.createElement('td'); td.textContent=value; tr.append(td);
    }
    const result=document.createElement('td');
    const label=document.createElement('span');
    label.textContent=p.last_error ? `${phaseName(p.last_error_phase)} failed` : p.attempts === 0 ? 'Not attempted by crawler' : p.last_peer_response != null && p.last_peer_response >= (p.last_attempt || 0) ? 'Peer response received' : 'Probe in progress / awaiting result';
    result.append(label);
    if(p.last_error){
      result.className='probe-error';
      const error=document.createElement('small');error.className='last-error';error.textContent=p.last_error;result.append(error);
      const retry=document.createElement('small');retry.textContent=`Failed ${date(p.last_error_at || p.last_attempt)} · next attempt ${date(p.next_attempt)}`;result.append(retry);
    }
    tr.append(result);
    const td=document.createElement('td'), details=document.createElement('details'), summary=document.createElement('summary'), pre=document.createElement('pre');
    details.open=open.has(p.endpoint); summary.textContent='Inspect'; pre.textContent=JSON.stringify(p,null,2); details.append(summary,pre); td.append(details);tr.append(td);body.append(tr);
  });
  if(!data.peers.length){const tr=document.createElement('tr'),td=document.createElement('td');td.colSpan=7;td.textContent='No matching observations yet.';tr.append(td);body.append(tr);}
  scroll.scrollTop=scrollTop;
  if(focused){Array.from(body.querySelectorAll('tr')).find(row=>row.dataset.endpoint===focused)?.querySelector('summary')?.focus({preventScroll:true});}
  text('count',`Page ${pageIndex+1} · ${data.peers.length} displayed · ${number(total)} matching endpoints`);
  el('previous').disabled=pageIndex===0;el('next').disabled=!nextCursor;
}
function fail(error) { text('status',`Update failed · ${error.message}. Retrying in 15 seconds.`); el('status').parentElement.classList.add('error'); }
async function refresh() {
  if(busy)return;busy=true;
  try {
    const [s,h]=await Promise.all([get('/api/stats'),get('/api/history')]);
    text('known',number(s.known));text('recent',number(s.recent));text('identities',number(s.unique_recent_identities));text('latency',ms(s.average_peer_request_ms));
    text('capacity',`${number(s.ever_connected)} ever connected · shared network catalog`);
    text('families',`${number(s.ipv4_recent)} IPv4 · ${number(s.ipv6_recent)} IPv6`);
    text('window',`${(s.recent_window_seconds/3600).toFixed(1)} hours`);
    const issues=[s.storage_error,s.seed_error].filter(Boolean);
    text('status',issues.length ? issues.join(' · ') : `${s.network} · ${s.inflight} probes in flight · ${number(s.due)} endpoints due`);
    el('status').parentElement.classList.toggle('error',issues.length>0);text('updated',`Updated ${date(s.timestamp)}`);
    const coverage=el('coverage');coverage.replaceChildren();
    for(const [label,value] of [['Attempted endpoints',s.attempted],['Not attempted',s.never_attempted],['Latest probe failed',s.failed_latest],['Waiting for retry',s.pending_retry]]){
      const item=document.createElement('div'),strong=document.createElement('strong'),span=document.createElement('span');strong.textContent=number(value);span.textContent=label;item.append(strong,span);coverage.append(item);
    }
    const success = s.totals.attempts ? `${(s.totals.connections/s.totals.attempts*100).toFixed(1)}% of attempts completed a handshake` : 'No completed probes yet';
    text('probe-budget',`${s.configured_concurrency} probe slots (shared across replicas) · starts spaced ${s.interval_ms/1000}s apart · ${s.timeout_seconds}s timeout · ${success}. IPv6: ${number(s.ipv6_known)} known, ${number(s.ipv6_attempted)} attempted, ${number(s.ipv6_recent)} recently connected.`);
    const phaseCounts=Object.entries(s.error_phases || {}).sort((a,b)=>b[1]-a[1]);
    text('failure-phases',phaseCounts.length ? `Latest failures by stage: ${phaseCounts.map(([phase,n])=>`${phaseName(phase)} ${number(n)}`).join(' · ')}` : 'No retained probe failures.');
    latestVersions={...s.versions};if(s.other_versions)latestVersions['Other reported versions']=s.other_versions;versions(latestVersions);series=h;chart();
    text('history-caption',h.length ? `${date(h[0].timestamp)} — ${date(h[h.length-1].timestamp)} · ${h.length} samples` : 'Waiting for samples.');
    const activity=el('activity');activity.replaceChildren();
    for(const [label,value] of [['Attempts',s.totals.attempts],['Connections',s.totals.connections],['Peer responses',s.totals.peer_responses],['Failures',s.totals.failures],['Gossip received',s.totals.gossip_received],['Rejected',s.totals.gossip_rejected],['Pruned',s.totals.pruned],['Snapshot',date(s.timestamp)]]){
      const item=document.createElement('div'),strong=document.createElement('strong'),span=document.createElement('span');strong.textContent=typeof value==='number'?number(value):value;span.textContent=label;item.append(strong,span);activity.append(item);
    }
    if(autoReload) await peers();
  }catch(error){fail(error);}finally{busy=false;}
}
el('previous').addEventListener('click',()=>{pageIndex=Math.max(0,pageIndex-1);peers().catch(fail);});
el('next').addEventListener('click',()=>{if(nextCursor){cursors[++pageIndex]=nextCursor;peers().catch(fail);}});
let debounce;
el('search').addEventListener('input',()=>{clearTimeout(debounce);peerGeneration++;debounce=setTimeout(()=>{pageIndex=0;cursors=[null];peers().catch(fail);},250);});
for(const id of ['sort','order','result-filter','family-filter']) el(id).addEventListener('change',()=>{
  if(id==='sort') el('order').value=['endpoint','version','ping'].includes(el('sort').value) ? 'asc' : 'desc';
  pageIndex=0;cursors=[null];peers().catch(fail);
});
el('auto-reload').addEventListener('click',()=>{
  autoReload=!autoReload;el('auto-reload').setAttribute('aria-pressed',String(autoReload));
  el('auto-reload').textContent=`Auto reload: ${autoReload ? 'on' : 'off'}`;
  if(autoReload) peers().catch(fail); else peerGeneration++;
});
el('reload').addEventListener('click',()=>peers().catch(fail));
window.addEventListener('resize',chart);
document.fonts.ready.then(chart);
window.addEventListener('themechange',()=>{chart();if(latestVersions)versions(latestVersions);});
refresh();setInterval(refresh,15000);
