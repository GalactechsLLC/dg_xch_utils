'use strict';
// Shared charts use real timestamps, preserve gaps, and expose exact values on focus or hover.
window.DruidChart = (() => {
  const states = new WeakMap();
  const compact = value => new Intl.NumberFormat(undefined, {notation:'compact',maximumFractionDigits:1}).format(value);
  const full = value => value.toLocaleString(undefined,{maximumFractionDigits:2});
  function render(canvas, options) {
    if (!canvas.clientWidth) {const existing=states.get(canvas);if(existing)existing.tooltip.hidden=true;return;}
    let state = states.get(canvas);
    if (!state) {
      let wrapper = canvas.parentElement;
      if(!wrapper.classList.contains('chart-wrap')){wrapper=document.createElement('div');wrapper.className='chart-wrap';canvas.before(wrapper);wrapper.append(canvas);}
      wrapper.classList.add('plot-container');
      const tooltip = wrapper.querySelector('.chart-tooltip') || document.createElement('div');
      tooltip.className = 'chart-tooltip'; tooltip.hidden = true; tooltip.setAttribute('role','status');
      canvas.after(tooltip); canvas.tabIndex = 0;
      state = {canvas, tooltip, selected:null}; states.set(canvas,state);
      const select = event => {
        if (!state.times?.length) return;
        const x = event.clientX - canvas.getBoundingClientRect().left;
        const t = state.start + (x-state.left)/(state.right-state.left)*(state.end-state.start);
        state.selected = state.times.reduce((a,b)=>Math.abs(a-t)<Math.abs(b-t)?a:b);
        paint(state);
      };
      canvas.addEventListener('pointermove',select); canvas.addEventListener('click',select);
      canvas.addEventListener('pointerleave',()=>{if(document.activeElement!==canvas){state.selected=null;paint(state);}});
      canvas.addEventListener('focus',()=>{state.selected=state.times?.at(-1)??null;paint(state);});
      canvas.addEventListener('blur',()=>{state.selected=null;paint(state);});
      canvas.addEventListener('keydown',event=>{
        if(!['ArrowLeft','ArrowRight','Home','End','Escape'].includes(event.key))return;
        event.preventDefault();
        if(event.key==='Escape')state.selected=null;
        else {const index=state.times.indexOf(state.selected);state.selected=state.times[event.key==='Home'?0:event.key==='End'?state.times.length-1:Math.max(0,Math.min(state.times.length-1,index+(event.key==='ArrowLeft'?-1:1)))];}
        paint(state);
      });
    }
    state.options = options; paint(state);
  }
  function paint(s) {
    const {canvas,options:o,tooltip}=s,w=canvas.clientWidth,h=290;
    if (!w) return;
    const dpr=window.devicePixelRatio||1;canvas.width=Math.round(w*dpr);canvas.height=h*dpr;
    const c=canvas.getContext('2d');c.scale(dpr,dpr);
    const theme=getComputedStyle(document.body),color=name=>theme.getPropertyValue(name).trim();
    c.font='11px Sora, sans-serif'; c.fillStyle=color('--muted'); tooltip.hidden=true;
    const groups=o.groups.map(g=>({...g,points:g.points.slice().sort((a,b)=>a.time-b.time)}));
    const points=groups.flatMap(g=>g.points).filter(p=>p.value!=null&&Number.isFinite(Number(p.value)));
    s.times=[...new Set(points.map(p=>p.time))].sort((a,b)=>a-b);
    if(!points.length){c.textAlign='center';c.fillText(o.empty||'No measurements in this range',w/2,136);canvas.setAttribute('aria-label',o.empty||'No measurements in this range');return;}
    s.start=o.start??Math.min(...points.map(p=>p.time));s.end=o.end??Math.max(...points.map(p=>p.time));
    if(s.start===s.end){s.start-=1800;s.end+=1800;}
    s.left=w<500?55:70;s.right=w-18;const top=30,bottom=h-48;
    const values=points.map(p=>Number(p.value));let low=o.zero===false?Math.min(...values):0,high=Math.max(...values);
    let span=Math.max(high-low,o.integer?1:Math.abs(high)*.02||1);
    if(o.zero===false){low=Math.max(0,low-span*.08);high+=span*.08;}
    const rough=Math.max(high-low,o.integer?1:span)/4,base=10**Math.floor(Math.log10(rough));
    const step=Math.max(o.integer?1:0,[1,2,2.5,5,10].find(n=>n*base>=rough)*base);
    low=Math.floor(low/step)*step;high=Math.max(low+step,Math.ceil(high/step)*step);
    const x=t=>s.left+(s.right-s.left)*(t-s.start)/(s.end-s.start),y=v=>bottom-(bottom-top)*(v-low)/(high-low);
    const format=o.format||full;
    let axis=o.axis||compact;
    const tickValues=[];for(let v=low;v<=high+step*.01;v+=step)tickValues.push(v);
    if(!o.axis&&new Set(tickValues.map(axis)).size<tickValues.length){
      const decimals=Math.max(0,Math.min(10,-Math.floor(Math.log10(step))+1));
      axis=v=>v.toLocaleString(undefined,{maximumFractionDigits:decimals});
    }
    s.left=Math.max(s.left,...tickValues.map(v=>c.measureText(axis(v)).width+14));
    c.textAlign='left';c.fillText(o.unit||'',s.left,13);
    for(let v=low;v<=high+step*.01;v+=step){const yy=y(v);c.strokeStyle=color('--line');c.lineWidth=.8;c.beginPath();c.moveTo(s.left,yy);c.lineTo(s.right,yy);c.stroke();c.fillStyle=color('--muted');c.textAlign='right';c.fillText(axis(v),s.left-9,yy+4);}
    const ticks=w<500?2:4,spanTime=s.end-s.start;
    for(let i=0;i<=ticks;i++){
      const t=s.start+spanTime*i/ticks,d=new Date(t*1000);c.textAlign=i===0?'left':i===ticks?'right':'center';
      if(spanTime>18*3600){c.fillText(d.toLocaleDateString(undefined,{month:'short',day:'numeric'}),x(t),h-24);if(spanTime<=2*86400)c.fillText(d.toLocaleTimeString(undefined,{hour:'2-digit',minute:'2-digit'}),x(t),h-9);}
      else c.fillText(d.toLocaleTimeString(undefined,{hour:'2-digit',minute:'2-digit',...(spanTime<600?{second:'2-digit'}:{})}),x(t),h-15);
    }
    groups.forEach((g,index)=>{
      c.strokeStyle=color(g.color||['--accent','--chart-blue','--chart-amber'][index%3]);c.fillStyle=c.strokeStyle;c.lineWidth=2.2;c.lineJoin='round';c.setLineDash(g.dashed?[5,4]:[]);c.beginPath();let prev=null;
      for(const p of g.points){if(p.value==null||!Number.isFinite(Number(p.value))){prev=null;continue;}
        if(!prev||p.time-prev.time>(o.gap||180))c.moveTo(x(p.time),y(p.value));else c.lineTo(x(p.time),y(p.value));prev=p;}
      c.stroke();c.setLineDash([]);
      g.points.forEach((p,i)=>{if(p.value==null||!Number.isFinite(Number(p.value)))return;const before=g.points[i-1],after=g.points[i+1];
        if(g.points.length<=12||((!before||before.value==null||p.time-before.time>(o.gap||180))&&(!after||after.value==null||after.time-p.time>(o.gap||180)))){c.beginPath();c.arc(x(p.time),y(p.value),3,0,Math.PI*2);c.fill();}});
    });
    canvas.setAttribute('aria-label',`${o.unit||'Measurements'}. ${groups.map(g=>g.name).join(', ')}. ${s.times.length} observed timestamps. Use arrow keys for exact values. Missing observations appear as gaps.`);
    if(s.selected==null)return;
    if(!s.times.includes(s.selected))s.selected=s.times.at(-1);
    const xx=x(s.selected);c.strokeStyle=color('--muted');c.setLineDash([3,4]);c.beginPath();c.moveTo(xx,top);c.lineTo(xx,bottom);c.stroke();c.setLineDash([]);
    tooltip.replaceChildren();const label=document.createElement('strong');label.textContent=new Date(s.selected*1000).toLocaleString();tooltip.append(label);
    for(const g of groups){const p=g.points.find(p=>p.time===s.selected),row=document.createElement('div');row.className='tooltip-value';const name=document.createElement('span'),value=document.createElement('b');name.textContent=g.name;value.textContent=p?.value==null?'Not observed':format(Number(p.value));row.append(name,value);tooltip.append(row);}
    tooltip.hidden=false;tooltip.style.left=`${Math.max(0,Math.min(w-tooltip.offsetWidth,xx> w/2?xx-tooltip.offsetWidth-12:xx+12))}px`;tooltip.style.top='32px';
  }
  return {render};
})();

const el = id => document.getElementById(id);
const number = value => value == null ? '—' : Number(value).toLocaleString(undefined,{maximumFractionDigits:2});
const date = seconds => seconds == null ? 'Not observed' : new Date(seconds * 1000).toLocaleString();
const time = seconds => new Date(seconds * 1000).toLocaleTimeString([], {hour:'2-digit', minute:'2-digit'});
const ms = value => value == null ? '—' : `${value.toFixed(1)} ms`;
const pct = (part, whole) => whole ? `${(part / whole * 100).toFixed(1)}%` : '—';
const phaseName = phase => ({tcp_connect:'TCP connection', tls:'TLS', websocket:'WebSocket', handshake:'Chia handshake', peer_request:'Peer request', verification:'Verification', unknown:'Probe'}[phase] || 'Probe');
function node(tag, text, cls) { const result=document.createElement(tag); if(text!=null)result.textContent=text; if(cls)result.className=cls; return result; }
function text(id, value) { el(id).textContent=value; }
async function get(path, signal) {
  const response=await fetch(path, {cache:'no-store', signal:signal ? AbortSignal.any([signal,AbortSignal.timeout(10000)]) : AbortSignal.timeout(10000)});
  if(!response.ok)throw new Error(`HTTP ${response.status}`);
  return response.json();
}
function ago(seconds) {
  if(seconds==null)return 'Never'; const delta=Math.max(0,Math.floor(Date.now()/1000-seconds));
  return delta<60 ? 'Just now' : delta<3600 ? `${Math.floor(delta/60)}m ago` : delta<86400 ? `${Math.floor(delta/3600)}h ago` : `${Math.floor(delta/86400)}d ago`;
}
function helpButton(label, message) { const button=node('button','?','help'); button.type='button';button.setAttribute('aria-label',label);button.setAttribute('aria-expanded','false');button.dataset.help=message;return button; }
// A single help surface supports mouse hover, keyboard focus, and pinned taps.
const helpSurface=el('help-popover'); let helpOwner=null, helpPinned=false, helpTimer;
function placeHelp() {
  if(!helpOwner||!helpOwner.isConnected){closeHelp();return;}
  const rect=helpOwner.getBoundingClientRect(), box=helpSurface.getBoundingClientRect();
  helpSurface.style.left=`${Math.max(12,Math.min(innerWidth-box.width-12,rect.left-box.width/2+rect.width/2))}px`;
  helpSurface.style.top=`${Math.max(12,Math.min(innerHeight-box.height-12,rect.bottom+10+box.height>innerHeight ? rect.top-box.height-10 : rect.bottom+10))}px`;
}
function closeHelp() { clearTimeout(helpTimer);if(helpOwner){helpOwner.setAttribute('aria-expanded','false');helpOwner.removeAttribute('aria-describedby');}helpOwner=null;helpPinned=false;helpSurface.hidden=true; }
function showHelp(button, pinned=false) {
  clearTimeout(helpTimer);if(helpPinned&&!pinned&&helpOwner!==button)return;
  if(helpOwner!==button)closeHelp();helpOwner=button;helpPinned=pinned||helpPinned;
  helpSurface.textContent=button.dataset.help;helpSurface.hidden=false;button.setAttribute('aria-expanded','true');button.setAttribute('aria-describedby','help-popover');placeHelp();
}
document.addEventListener('pointerover',event=>{const button=event.target.closest?.('.help');if(button)showHelp(button);else if(helpSurface.contains(event.target))clearTimeout(helpTimer);});
document.addEventListener('pointerout',event=>{if(helpPinned)return;if(event.target.closest?.('.help')||helpSurface.contains(event.target))helpTimer=setTimeout(()=>{if(document.activeElement!==helpOwner)closeHelp();},180);});
document.addEventListener('focusin',event=>{if(event.target.matches('.help'))showHelp(event.target);});
document.addEventListener('focusout',event=>{if(event.target===helpOwner&&!helpPinned)closeHelp();});
document.addEventListener('click',event=>{const button=event.target.closest?.('.help');if(button){if(helpOwner===button&&helpPinned)closeHelp();else showHelp(button,true);}else if(!helpSurface.contains(event.target))closeHelp();});
document.addEventListener('keydown',event=>{if(event.key==='Escape'){closeHelp();selectedSample=null;drawChart();}});
window.addEventListener('scroll',()=>{if(helpOwner)placeHelp();},{passive:true});
document.querySelectorAll('.help').forEach(button=>button.setAttribute('aria-expanded','false'));

let latestSummary=null, historyError=null, series=[], chartMode='reachability', selectedSample=null, chartSamples=[], chartGeometry=null, refreshing=false, refreshPending=false;
const hiddenSeries=new Set();
const charts={
  gossip:[['total_nodes_5_days','Unique IPs · 5 days','green'],['ipv4_nodes_5_days','IPv4 · 5 days','blue'],['ipv6_nodes_5_days','IPv6 · 5 days','amber']],
  inventory:[['known','Retained endpoints','green'],['recent','Recently connected','blue'],['identities','Recent identities','amber']],
  reachability:[['recent','Recently connected','blue'],['identities','Recent identities','amber']],
  activity:[['completed_probes','Completed probes','green'],['handshakes','Handshakes','blue'],['peer_responses','Peer responses','amber'],['failures','Failed probes','red']],
  latency:[['average_connect_ms','Connection time','green'],['average_peer_request_ms','Peer response time','blue']]
};
const colorVariable={green:'--accent',blue:'--chart-blue',amber:'--chart-amber',red:'--error'};
const chartDescriptions={gossip:'Unique IPs from accepted gossip with an advertised timestamp in the previous five days. Connections are not required.',inventory:'Retained IP-and-port endpoints, including seeds and older gossip, compared with recent connections.',reachability:'Endpoints and certificate identities observed during the recent connection window.',activity:'Completed probe results per minute, including retries.',latency:'Mean measured connection and peer-response times per minute.'};
function historyBucket(){const range=Number(el('history-range').value);return range<=86400?60:range<=604800?600:7200;}
function formatSample(key,value) { return key.endsWith('_ms') ? ms(value) : value==null ? '—' : number(value); }
function chartControls() {
  text('chart-description',chartMode==='activity'?`Completed results per minute, averaged over ${historyBucket()/60}-minute buckets. Retries are included; handshakes and failures can overlap.`:chartMode==='latency'?`Mean connection and peer-response times per ${historyBucket()/60}-minute bucket.`:chartDescriptions[chartMode]);
  const legend=el('chart-legend');legend.replaceChildren();
  charts[chartMode].forEach(([key,label,color])=>{
    const button=node('button');button.type='button';button.append(node('span',null,`series-dot ${color}`),node('span',label));button.setAttribute('aria-pressed',String(!hiddenSeries.has(key)));
    button.addEventListener('click',()=>{if(hiddenSeries.has(key))hiddenSeries.delete(key);else hiddenSeries.add(key);chartControls();drawChart();});legend.append(button);
  });
}
function chartWindow() {
  if(!series.length)return [];
  const end=Math.max(Math.floor(Date.now()/1000),series.at(-1).timestamp);
  return series.filter(point=>point.timestamp>=end-Number(el('history-range').value));
}
function drawChart() {
  const samples=chartWindow().filter(p=>!['activity','latency'].includes(chartMode)||p.timestamp+(p.interval_seconds||historyBucket())<=Date.now()/1000);
  const visible=charts[chartMode].filter(([key])=>!hiddenSeries.has(key));
  const value=(p,key)=>p[key]==null?null:chartMode==='activity'?p[key]*60/(p.interval_seconds||historyBucket()):p[key];
  const last=samples.at(-1),highlights=el('chart-highlights');highlights.replaceChildren();
  visible.forEach(([key,label])=>{const item=node('div');item.append(node('strong',last?formatSample(key,value(last,key)):'—'),node('small',chartMode==='activity'?`${label} / min`:label));highlights.append(item);});
  const end=Math.max(Date.now()/1000,series.at(-1)?.timestamp||0);
  window.DruidChart.render(el('history'),{
    groups:visible.map(([key,name,col])=>({name,color:colorVariable[col],dashed:key==='identities',points:samples.map(p=>({time:p.timestamp,value:value(p,key)}))})),
    start:end-Number(el('history-range').value),end,gap:historyBucket()*1.5,
    unit:chartMode==='latency'?'Milliseconds':chartMode==='activity'?'Completed results / minute':chartMode==='gossip'?'Distinct IP addresses':'Endpoints / identities',
    format:v=>chartMode==='latency'?ms(v):Number(v).toLocaleString(undefined,{maximumFractionDigits:2}),integer:!['latency','activity'].includes(chartMode),
    empty:historyError?'History is temporarily unavailable':visible.length?'No measurements in this range':'Select a series below to display it'
  });
  text('history-caption',historyError||`${samples.length.toLocaleString()} observed buckets · ${historyBucket()/60} min resolution${['activity','latency'].includes(chartMode)?' · completed buckets only':''}`);
}
document.querySelectorAll('[data-chart]').forEach(button=>button.addEventListener('click',()=>{chartMode=button.dataset.chart;selectedSample=null;document.querySelectorAll('[data-chart]').forEach(b=>b.setAttribute('aria-pressed',String(b===button)));chartControls();drawChart();}));
el('history-range').addEventListener('change',()=>{selectedSample=null;chartControls();refresh();drawChart();});

let versionEntries=[],versionData=null,versionExpanded=false,versionBusy=false,versionUpdated=0;
function drawVersions() {
  const root=el('versions'),scrollTop=root.scrollTop,focusedVersion=root.contains(document.activeElement)?document.activeElement.dataset.version:null;root.replaceChildren();root.closest('.versions-panel').classList.toggle('expanded',versionExpanded);
  if(!versionEntries.length){text('versions-caption','No recent versions reported');root.append(node('div','No recent versions reported yet.','version-empty'));el('versions-toggle').hidden=true;el('versions-more').hidden=true;return;}
  const visible=versionExpanded?versionEntries:versionEntries.slice(0,6);
  for(const {version,count} of visible){const row=node('div',null,'version'),label=node('div',null,'version-label'),button=node('button',version||'Version not reported');button.type='button';button.dataset.version=version;button.setAttribute('aria-label',`Inspect endpoints reporting ${version||'no version'}`);button.addEventListener('click',()=>filterPeers({version,status:'connected'}));label.append(button,node('span',`${number(count)} · ${pct(count,versionData.total_endpoints)}`));
    const track=node('div',null,'track'),fill=node('canvas',null,'fill');fill.width=300;fill.height=5;const ctx=fill.getContext('2d');ctx.fillStyle=getComputedStyle(document.body).getPropertyValue('--accent').trim();ctx.fillRect(0,0,300*count/Math.max(1,versionData.total_endpoints),5);track.append(fill);row.append(label,track);root.append(row);
  }
  root.scrollTop=scrollTop;if(focusedVersion!=null)Array.from(root.querySelectorAll('button')).find(button=>button.dataset.version===focusedVersion)?.focus({preventScroll:true});
  text('versions-caption',`${number(versionData.total_versions)} versions · ${number(versionData.total_endpoints)} endpoints`);
  el('versions-caption').title=`Recently connected endpoints. Updated ${date(versionData.timestamp)}.`;
  el('versions-toggle').hidden=versionData.total_versions<=6;el('versions-toggle').setAttribute('aria-expanded',String(versionExpanded));el('versions-toggle').textContent=versionExpanded?'Show fewer':`${versionData.total_versions>versionEntries.length?'Browse':'Show'} all ${number(versionData.total_versions)} versions`;
  el('versions-more').hidden=!versionExpanded||versionData.next_offset==null;el('versions-more').textContent=`Load more · ${number(versionEntries.length)} of ${number(versionData.total_versions)} shown`;
}
async function loadVersions(more=false) {
  if(versionBusy)return;versionBusy=true;el('versions-reload').disabled=true;el('versions-more').disabled=true;
  try {
    const wanted=versionEntries.length;let offset=more?versionData.next_offset:0,data;const entries=more?[...versionEntries]:[];
    do{data=await get(`/api/versions?limit=100&offset=${offset}`);entries.push(...data.versions);offset=data.next_offset;}while(!more&&offset!=null&&entries.length<wanted);
    versionEntries=Array.from(new Map(entries.map(entry=>[entry.version,entry])).values());versionData=data;versionUpdated=Date.now();drawVersions();
  }
  catch(error){text('versions-caption',`Versions unavailable · ${error.message}`);}
  finally {versionBusy=false;el('versions-reload').disabled=false;el('versions-more').disabled=false;}
}
el('versions-toggle').addEventListener('click',()=>{versionExpanded=!versionExpanded;drawVersions();});
el('versions-more').addEventListener('click',()=>loadVersions(true));
el('versions-reload').addEventListener('click',()=>loadVersions());

const errorDescriptions={
  refused:['Connection refused','The TCP connection was actively refused. Common causes include a closed listening port, an offline node, or a firewall rejection. The recorded message is shown below; refusal does not identify the exact cause.'],
  timeout:['Timed out','The stage did not finish before the probe deadline. Stale addresses, offline nodes, dropped traffic, firewalls, or NAT can cause this. A timeout alone cannot prove missing port forwarding.'],
  routing:['Route unavailable','The operating system reported an unreachable network or host. Check outbound access and IPv6 routing on the crawler host. This error can originate on the crawler side.'],
  permission:['Socket blocked locally','The operating system denied the connection. Check the crawler host or container network policy; this does not show whether the remote node is offline.'],
  local_address:['Local address unavailable','The crawler could not select a usable local address. Check host interfaces, IPv6 configuration, and outbound socket resources.'],
  connection:['Connection error','The TCP socket failed before TLS or the Chia handshake. Inspect the recorded operating-system message to narrow down the cause.'],
  handshake:['Handshake rejected','The endpoint reported a different network or unsupported node type. This is a protocol mismatch rather than evidence of a closed TCP port.'],
  closed:['Peer closed the connection','The connection closed or reset during this stage. A peer limit, firewall, restart, or protocol policy can cause this; inspect the recorded error.'],
  protocol:['Protocol error','The TCP connection reached a later stage, which failed. Inspect the TLS, WebSocket, handshake, or peer-request error instead of assuming a port-forwarding problem.']
};
function drawDiagnostics(data) {
  const root=el('failure-groups');root.replaceChildren();
  if(!data.latest_failures){text('failure-summary','No retained probe failures.');text('failure-caption','Failures appear here after an unsuccessful probe.');return;}
  text('failure-summary',`${number(data.latest_failures)} ${data.latest_failures===1?'endpoint':'endpoints'} with a latest failure`);
  for(const group of data.groups){const [label,description]=errorDescriptions[group.reason]||errorDescriptions.protocol,row=node('div',null,'failure-group'),button=node('button',null,'error-link');button.type='button';button.append(node('span',label),node('small',`${phaseName(group.phase)} · ${number(group.ipv4)} IPv4 / ${number(group.ipv6)} IPv6`));button.addEventListener('click',()=>filterPeers({status:'failed',failure:{phase:group.phase,reason:group.reason}}));row.append(button,node('strong',number(group.count)),helpButton(`About ${label}`,`${description}\n\n${group.error_variants>1?`Example of ${number(group.error_variants)} recorded messages`:'Recorded error'}: ${group.error}`));root.append(row);}
  text('failure-caption',data.other_failures ? `${number(data.other_failures)} additional failures · select an error to inspect` : 'Latest results · select an error to inspect');
}
function drawCoverage() {
  if(!latestSummary)return;
  const s=latestSummary,root=el('coverage');root.replaceChildren();
  const values=[['Attempted',s.attempted,'Distinct retained endpoints that have had at least one crawler attempt. Retries do not increase this number.'],['Not attempted',s.never_attempted,'Discovered endpoints that have not had a crawler attempt yet. Discovery can grow faster than polite probe scheduling.'],['Latest failure',s.failed_latest,'Endpoints with a recorded failure from their latest completed probe. This clears on a complete successful probe; it can remain while a retry is in flight.'],['Waiting for retry',s.pending_retry,'Failed endpoints with a future retry deadline and no current lease. Other failures can already be due for another attempt.']];
  for(const [label,value,description] of values){const item=node('div',null,'coverage-item'),heading=node('div',null,'coverage-value');heading.append(node('strong',number(value)),helpButton(`About ${label}`,description));item.append(heading,node('small',label));root.append(item);}
  const bar=el('coverage-track'),ratio=devicePixelRatio||1,width=bar.clientWidth;bar.width=width*ratio;bar.height=9*ratio;const ctx=bar.getContext('2d');ctx.scale(ratio,ratio);const theme=getComputedStyle(document.body);
  const connected=s.attempted_connected,segments=[[connected,'--accent'],[Math.max(0,s.attempted-connected),'--error'],[s.never_attempted,'--queued']];let x=0;
  for(const [count,color] of segments){const size=width*count/Math.max(1,s.known);ctx.fillStyle=theme.getPropertyValue(color).trim();ctx.fillRect(x,0,size,9);x+=size;}
  bar.setAttribute('aria-label',`${number(connected)} attempted endpoints with a connection, ${number(Math.max(0,s.attempted-connected))} attempted without a connection, ${number(s.never_attempted)} not attempted`);
  const budget=el('probe-budget');budget.replaceChildren();for(const value of [`${pct(s.attempted,s.known)} attempted`,`${s.inflight} / ${s.configured_concurrency} probes active`,`${s.interval_ms/1000}s start spacing`,`${s.tcp_connect_timeout_seconds}s TCP deadline`,`${s.timeout_seconds}s connection deadline`,`${s.peak_wait_ms/1000}s peak wait`,`${number(s.ipv6_attempted)} IPv6 attempted`])budget.append(node('span',value));
  const ipv6Disabled=s.enable_ipv6===false;el('ipv6-policy-note').hidden=!ipv6Disabled;if(ipv6Disabled)budget.append(node('span','IPv6 probing disabled'));
}

let pageIndex=0,cursors=[null],nextCursor=null,total=0,peerGeneration=0,peerBusy=false,peerAbort=null,pendingAutomatic=false,autoReload=true;
const limit=50,inspections=new Set(),rawInspections=new Set();let currentPeers=[],selectedVersion=null,selectedFailure=null;
function paginationState() {el('previous').disabled=peerBusy||pageIndex===0;el('next').disabled=peerBusy||!nextCursor;el('reload').disabled=peerBusy;el('peer-list').setAttribute('aria-busy',String(peerBusy));}
function inspector(peer) {
  const tr=node('tr',null,'detail-row');tr.dataset.inspection=peer.endpoint;const td=node('td');td.colSpan=8;const panel=node('section',null,'node-inspector');panel.setAttribute('aria-label',`Inspection of ${peer.endpoint}`);
  const heading=node('div',null,'inspector-heading');heading.append(node('strong',peer.endpoint));const copy=node('button','Copy endpoint','secondary copy-endpoint');copy.type='button';copy.addEventListener('click',()=>copyText(peer.endpoint));heading.append(copy);panel.append(heading);
  const grid=node('div',null,'inspector-grid');
  const lists=[['Connection',[['Version',peer.software_version||'Not reported'],['Protocol',peer.protocol_version||'Not reported'],['Negotiated TLS',peer.tls_version||'Not observed'],['Node type',({1:'Full node',5:'Introducer'})[peer.node_type]||'Not reported'],['Connection time',ms(peer.connect_ms)],['Peer response time',ms(peer.peer_request_ms)],['Attempts / successful peer requests',`${number(peer.attempts)} / ${number(peer.successes)}`]]],['Discovery & timeline',[['First discovered',date(peer.first_seen)],['First source',peer.first_source||'Not observed'],['First gossip received',date(peer.first_gossiped)],['Last gossip received',date(peer.last_gossiped)],['Last gossiped by',peer.last_gossiped_by||'Not observed'],['Advertised peer timestamp',date(peer.reported_gossip_timestamp)],['First connection',date(peer.first_success)],['Last connection',date(peer.last_success)],['Last peer response',date(peer.last_peer_response)]]],['Latest probe',[['Last attempt',date(peer.last_attempt)],['Next scheduled attempt',date(peer.next_attempt)],['Consecutive failures',number(peer.consecutive_failures)],['Failure stage',peer.last_error?phaseName(peer.last_error_phase):'None'],['Last failure',peer.last_error||'None'],['Reliable full node',peer.reliable && peer.node_type===1 ? 'Yes' : 'No'],['Reliability successes / observations',`${number(peer.reliability?.successes)} / ${number(peer.reliability?.tries)}`],['Ignored until',date(peer.reliability?.ignore_until||null)],['Suspended until',date(peer.reliability?.ban_until||null)],['Last Known Height',peer.peak_height==null?'Not observed':number(peer.peak_height)],['Height last observed',date(peer.peak_observed_at)]]]];
  for(const [title,fields] of lists){const section=node('div'),dl=node('dl');section.append(node('h3',title));for(const [label,value] of fields){const row=node('div');row.append(node('dt',label),node('dd',value));dl.append(row);}section.append(dl);grid.append(section);}panel.append(grid);
  const identity=node('div',null,'inspector-identity');identity.append(node('small','Certificate identity'),node('code',peer.node_id||'Not observed'));panel.append(identity);
  if(peer.capabilities.length)panel.append(node('small',`Reported capabilities: ${peer.capabilities.map(([id,value])=>`${id}: ${value}`).join(' · ')}`));
  const raw=node('details');raw.open=rawInspections.has(peer.endpoint);raw.addEventListener('toggle',()=>{if(raw.isConnected){if(raw.open)rawInspections.add(peer.endpoint);else rawInspections.delete(peer.endpoint);}});raw.append(node('summary','Raw observation'),node('pre',JSON.stringify(peer,null,2)));panel.append(raw);td.append(panel);tr.append(td);return tr;
}
async function copyText(value) {try{await navigator.clipboard.writeText(value);text('table-message','Endpoint copied.');}catch{text('table-message','Copy is unavailable here. Select the endpoint text to copy it.');}}
function drawPeers() {
  const body=el('peers'),scroll=el('peer-list'),scrollTop=scroll.scrollTop,focusedRow=document.activeElement?.closest('tr[data-endpoint],tr[data-inspection]'),focused=focusedRow?.dataset.endpoint||focusedRow?.dataset.inspection,focusedControl=document.activeElement?.matches('.node-inspector summary')?'summary':document.activeElement?.matches('.copy-endpoint')?'.copy-endpoint':'.inspect-button';
  for(const endpoint of inspections)if(!currentPeers.some(peer=>peer.endpoint===endpoint)){inspections.delete(endpoint);rawInspections.delete(endpoint);}
  body.replaceChildren();
  for(const peer of currentPeers){const tr=node('tr');tr.dataset.endpoint=peer.endpoint;
    const values=[peer.endpoint,peer.software_version||'—',ago(peer.first_seen),ago(peer.last_success),peer.peak_height==null?'—':number(peer.peak_height),ms(peer.peer_request_ms)];
    values.forEach((value,index)=>{const td=node('td',value,index===0?'endpoint':null);if(index===2)td.title=date(peer.first_seen);if(index===3)td.title=date(peer.last_success);if(index===4)td.title=peer.peak_observed_at?`Height observed ${date(peer.peak_observed_at)}`:'No peak observed';tr.append(td);});
    const result=node('td'),success=!peer.last_error&&peer.last_peer_response!=null,badge=node('span',peer.inflight?'Probing now':peer.last_error?`${phaseName(peer.last_error_phase)} failed`:peer.attempts===0?'Not attempted':success?'Peer response received':'Awaiting a result',`result-badge ${peer.last_error&&!peer.inflight?'failure':success?'success':''}`);result.append(badge);
    if(peer.last_error){result.append(node('small',peer.last_error));if(!peer.inflight)result.append(node('small',`Next attempt ${date(peer.next_attempt)}`));}
    tr.append(result);const td=node('td'),button=node('button',inspections.has(peer.endpoint)?'Close':'Inspect','inspect-button');button.type='button';button.setAttribute('aria-expanded',String(inspections.has(peer.endpoint)));button.setAttribute('aria-label',`${inspections.has(peer.endpoint)?'Close inspection of':'Inspect'} ${peer.endpoint}`);
    button.addEventListener('click',()=>{if(inspections.has(peer.endpoint)){inspections.delete(peer.endpoint);rawInspections.delete(peer.endpoint);}else inspections.add(peer.endpoint);drawPeers();Array.from(body.querySelectorAll('tr[data-endpoint]')).find(row=>row.dataset.endpoint===peer.endpoint)?.querySelector('.inspect-button').focus({preventScroll:true});});td.append(button);tr.append(td);body.append(tr);if(inspections.has(peer.endpoint))body.append(inspector(peer));
  }
  if(!currentPeers.length){const row=node('tr'),td=node('td','No matching endpoints. Try clearing the filters.','empty');td.colSpan=8;row.append(td);body.append(row);}
  scroll.scrollTop=scrollTop;if(focused){const row=Array.from(body.querySelectorAll(focusedControl==='.inspect-button'?'tr[data-endpoint]':'tr[data-inspection]')).find(row=>(row.dataset.endpoint||row.dataset.inspection)===focused);row?.querySelector(focusedControl)?.focus({preventScroll:true});}
  text('count',`Page ${pageIndex+1} · ${currentPeers.length} displayed · ${number(total)} matching endpoints`);paginationState();
}
async function peers(targetIndex=pageIndex,automatic=false) {
  if(automatic&&peerBusy)return;
  peerAbort?.abort();const controller=new AbortController();peerAbort=controller;const generation=++peerGeneration;peerBusy=true;pendingAutomatic=automatic;paginationState();
  const query=new URLSearchParams({limit,search:el('search').value,sort:el('sort').value,order:el('order').value,status:el('result-filter').value,family:el('family-filter').value});if(selectedFailure){query.set('failure_phase',selectedFailure.phase);query.set('failure_reason',selectedFailure.reason);}if(cursors[targetIndex])query.set('cursor',cursors[targetIndex]);if(selectedVersion!==null){query.set('version',selectedVersion);const recent=versionData?.recent_window_seconds??latestSummary?.recent_window_seconds;if(recent!=null)query.set('recent_seconds',recent);}
  try{const data=await get(`/api/peers?${query}`,controller.signal);if(generation!==peerGeneration)return;
    if(!data.peers.length&&targetIndex>0){await peers(targetIndex-1,false);return;}
    pageIndex=targetIndex;nextCursor=data.next_cursor;total=data.total;currentPeers=data.peers;drawPeers();text('table-message',autoReload?'List updates every 15 seconds.':'List paused. Overview and charts remain live.');
  }catch(error){if(generation===peerGeneration&&error.name!=='AbortError'){text('table-message',`List update failed · ${error.message}.${currentPeers.length?' Your current page is preserved.':' Try Refresh list.'}`);if(!currentPeers.length){const row=node('tr'),cell=node('td','Endpoint list unavailable. Use Refresh list to try again.','empty');cell.colSpan=8;row.append(cell);el('peers').replaceChildren(row);text('count','Endpoints unavailable');}}}
  finally{if(generation===peerGeneration){peerBusy=false;pendingAutomatic=false;paginationState();}}
}
function resetList() {pageIndex=0;cursors=[null];nextCursor=null;el('clear-filters').hidden=selectedFailure===null&&selectedVersion===null&&!el('search').value&&el('result-filter').value==='all'&&el('family-filter').value==='all';const active=el('active-filter');active.hidden=selectedVersion===null&&selectedFailure===null;active.textContent=selectedFailure?`${(errorDescriptions[selectedFailure.reason]||errorDescriptions.protocol)[0]} · ${phaseName(selectedFailure.phase)} · latest failures`:selectedVersion===null?'':`Version: ${selectedVersion||'not reported'} · recent connections`;return peers(0);}
function filterPeers({search='',status='all',version=null,failure=null}) {clearTimeout(searchTimer);selectedVersion=version;selectedFailure=failure;el('search').value=search;el('result-filter').value=status;el('family-filter').value='all';resetList();location.hash='peers';showView(false);el('observations').scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'auto':'smooth',block:'start'});}
el('previous').addEventListener('click',()=>{if(!peerBusy&&pageIndex>0)peers(pageIndex-1);});
el('next').addEventListener('click',()=>{if(!peerBusy&&nextCursor){cursors[pageIndex+1]=nextCursor;peers(pageIndex+1);}});
let searchTimer;
el('search').addEventListener('input',()=>{clearTimeout(searchTimer);peerAbort?.abort();peerGeneration++;peerBusy=false;nextCursor=null;paginationState();searchTimer=setTimeout(resetList,250);});
for(const id of ['sort','order','result-filter','family-filter'])el(id).addEventListener('change',()=>{if(id==='result-filter'){if(el(id).value!=='failed')selectedFailure=null;if(el(id).value!=='connected')selectedVersion=null;}if(id==='sort')el('order').value=['endpoint','version','ping'].includes(el('sort').value)?'asc':'desc';resetList();});
el('clear-filters').addEventListener('click',()=>{clearTimeout(searchTimer);el('search').value='';el('result-filter').value='all';el('family-filter').value='all';selectedVersion=null;selectedFailure=null;resetList();});
el('auto-reload').addEventListener('click',()=>{autoReload=!autoReload;el('auto-reload').setAttribute('aria-pressed',String(autoReload));el('auto-reload').textContent=`Auto reload: ${autoReload?'on':'off'}`;if(autoReload)peers();else{if(pendingAutomatic){peerAbort?.abort();peerGeneration++;peerBusy=false;paginationState();}text('table-message','List paused. Overview and charts remain live.');}});
el('reload').addEventListener('click',()=>peers());

function drawSummary(s) {
  latestSummary=s;
  const gossipAvailable=s.total_nodes_5_days!=null;
  text('gossip-count',number(s.total_nodes_5_days));text('gossip-families',gossipAvailable?`${number(s.ipv4_nodes_5_days)} IPv4 · ${number(s.ipv6_nodes_5_days)} IPv6`:'Five-day counts unavailable');text('gossip-window',gossipAvailable?'By advertised peer timestamp':'Waiting for five-day discovery statistics');
  text('network',s.network);text('known',number(s.known));text('recent',number(s.recent));text('identities',number(s.unique_recent_identities));text('latency',ms(s.average_peer_request_ms));text('capacity',`${number(s.reliable)} reliable full nodes · ${number(s.ever_connected)} connected at least once`);text('families',`${number(s.ipv4_recent)} IPv4 · ${number(s.ipv6_recent)} IPv6`);
  el('recent-help').dataset.help=`Endpoints that completed a valid Chia handshake within the last ${(s.recent_window_seconds/3600).toFixed(1)} hours. The crawler disconnects after probing; a recent connection does not guarantee the node is currently online or synced.`;
  const issues=[s.storage_error,s.seed_error].filter(Boolean);text('status',issues.length?issues.join(' · '):`${s.network} · ${s.inflight} probes active · ${number(s.due)} scheduled endpoints due`);el('status').parentElement.classList.toggle('error',issues.length>0);text('updated',`Updated ${time(s.timestamp)} · live every 15s`);
  drawCoverage();
  const activity=el('activity');activity.replaceChildren();for(const [label,value] of [['Reliable full nodes',s.reliable],['Temporarily ignored',s.ignored],['Suspended',s.banned],['Attempts',s.totals.attempts],['Handshakes',s.totals.connections],['Peer responses',s.totals.peer_responses],['Failed probes',s.totals.failures],['Raw gossip entries · cumulative',s.totals.gossip_received],['Rejected entries',s.totals.gossip_rejected],['Pruned endpoints',s.totals.pruned],['Retained IPv6 endpoints',s.ipv6_known]]){const item=node('div');item.append(node('strong',number(value)),node('small',label));activity.append(item);}
}
async function refresh() {
  if(refreshing){refreshPending=true;return;}refreshing=true;
  // Keep independent panels live when one endpoint is unavailable.
  const updates=[
    get('/api/stats').then(drawSummary).catch(error=>{text('status',`Overview unavailable · ${error.message}. Retrying shortly.`);el('status').parentElement.classList.add('error');if(!latestSummary)text('network','Unavailable');}),
    get(`/api/history?days=${Math.max(1,Math.ceil(Number(el('history-range').value)/86400))}`).then(history=>{historyError=null;series=history.sort((a,b)=>a.timestamp-b.timestamp).map(p=>p.activity_observed===false?{...p,completed_probes:null,handshakes:null,peer_responses:null,failures:null}:p);drawChart();}).catch(error=>{historyError=`History unavailable · ${error.message}${series.length?' · showing previous observations':''}`;drawChart();}),
    get('/api/diagnostics').then(drawDiagnostics).catch(error=>{text('failure-summary',`Diagnostics unavailable · ${error.message}`);text('failure-caption',el('failure-groups').childElementCount?'Showing previous results. Retrying shortly.':'Retrying shortly.');})
  ];
  if(autoReload&&activeView==='peers')updates.push(peers(pageIndex,true));
  if(!versionData||Date.now()-versionUpdated>60000)updates.push(loadVersions());
  try{await Promise.allSettled(updates);}finally{refreshing=false;if(refreshPending){refreshPending=false;refresh();}}
}
window.addEventListener('resize',()=>{drawChart();drawCoverage();if(helpOwner)placeHelp();});
window.addEventListener('themechange',()=>{drawChart();drawCoverage();drawVersions();});
document.fonts.ready.then(()=>{drawChart();drawCoverage();});

const viewMeta={overview:['Network overview','Peer discovery, reachability, and activity at a glance.'],blockchain:['Blockchain','Source health, chain progress, and independently checked observations.'],network:['Network composition','Explore the software and networks behind recent connections.'],peers:['Peer explorer','Find an endpoint and inspect what the crawler observed.'],crawler:['Crawler health','Understand probe outcomes, coverage, and collection quality.']};
let activeView='overview';
function showView(focus=true) {
  const aliases={chain:'blockchain',geography:'network',trends:'overview',observations:'peers'};
  const hash=location.hash.slice(1),next=aliases[hash]||hash;
  activeView=Object.hasOwn(viewMeta,next)?next:'overview';
  document.querySelectorAll('[data-view]').forEach(panel=>{panel.hidden=panel.dataset.view!==activeView;});
  document.querySelectorAll('[data-view-link]').forEach(link=>{if(link.dataset.viewLink===activeView)link.setAttribute('aria-current','page');else link.removeAttribute('aria-current');});
  text('view-title',viewMeta[activeView][0]);text('view-description',viewMeta[activeView][1]);document.title=`${viewMeta[activeView][0]} · Druid Garden`;
  closeHelp();drawChart();drawCoverage();drawVersions();window.dispatchEvent(new Event('viewchange'));
  if(activeView==='peers')peers(pageIndex,true);
  if(focus){window.scrollTo({top:0,behavior:'instant'});el('view-title').focus({preventScroll:true});}
}
window.addEventListener('hashchange',()=>showView());
chartControls();showView(false);refresh();setInterval(()=>{if(!document.hidden)refresh();},15000);
