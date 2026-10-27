'use strict';
(() => {
  const $=id=>document.getElementById(id);
  const make=(tag,value,cls)=>{const e=document.createElement(tag);if(value!=null)e.textContent=value;if(cls)e.className=cls;return e;};
  const fmt=v=>v==null?'—':Number(v).toLocaleString(undefined,Math.abs(Number(v))>0&&Math.abs(Number(v))<.01?{maximumSignificantDigits:3}:{maximumFractionDigits:2});
  const stamp=v=>v==null?'Not observed':new Date(Number(v)*1000).toLocaleString();
  const spaceUnits=['B','KiB','MiB','GiB','TiB','PiB','EiB'];
  const spacePower=value=>Math.max(0,Math.min(spaceUnits.length-1,Math.floor(Math.log2(Math.max(1,Number(value)))/10)));
  const space=v=>v==null?'—':`${fmt(Number(v)/1024**spacePower(v))} ${spaceUnits[spacePower(v)]}`;
  const unit=(value,suffix)=>value==null?'—':`${fmt(value)} ${suffix}`;
  let chain=null,insights=null,busy=false,pending=false;
  const expanded=new Set();
  async function load(url){const r=await fetch(url,{signal:AbortSignal.timeout(15000)});if(!r.ok)throw new Error(`HTTP ${r.status}`);return r.json();}
  function cards(id,values){const host=$(id);host.replaceChildren();for(const [label,value] of values){const e=make('div');e.append(make('small',label),make('strong',value));host.append(e);}}
  function ranking(id,rows,total){
    const host=$(id);host.classList.add('rank-list');host.replaceChildren();
    const shown=expanded.has(id)?rows:rows.slice(0,8);
    for(const [label,count] of shown){
      const e=make('div',null,'rank-row'),heading=make('div',null,'rank-heading'),name=make('span',label);name.title=label;
      heading.append(name,make('strong',`${fmt(count)}${total?' · '+fmt(count/total*100)+'%':''}`));e.append(heading);
      const meter=make('meter');meter.min=0;meter.max=Math.max(1,total||Math.max(...rows.map(r=>r[1])));meter.value=count;meter.setAttribute('aria-label',`${label}: ${fmt(count)}`);e.append(meter);host.append(e);
    }
    if(!rows.length)host.append(make('p','No observations yet.','empty-state'));
    if(rows.length>8){const button=make('button',expanded.has(id)?'Show fewer':`Show all ${rows.length}`,'text-button');button.type='button';button.setAttribute('aria-expanded',String(expanded.has(id)));button.onclick=()=>{if(expanded.has(id))expanded.delete(id);else expanded.add(id);ranking(id,rows,total);};host.append(button);}
  }
  function chartLegend(id,groups){const host=$(id);host.replaceChildren();groups.forEach((g,i)=>{const label=make('span',null,'legend-item');label.append(make('span',null,`series-dot ${['green','blue','amber'][i%3]}`),make('span',g.name));host.append(label);});}
  function chainMessage(){
    const nodes=chain.nodes||[],available=nodes.filter(n=>n.available);
    if(!nodes.length)return ['No blockchain sources configured','Peer discovery is available. Blockchain measurements will appear when RPC sources are connected.'];
    if(!available.length)return ['Blockchain sources unavailable','The crawler cannot connect to its RPC sources. Check source TLS certificates and connectivity. Peer discovery is still available.'];
    if(!chain.fresh)return ['Blockchain observations are stale','The last source measurements are shown below. They do not describe the current chain.'];
    if(nodes.length<2)return ['One source connected','Source measurements are shown below. An independently checked chain index requires two synced sources that agree.'];
    if(!chain.quorum)return ['Awaiting source agreement','Two synced sources must agree on the same checkpoint before indexed chain statistics are published.'];
    return [`Checkpoint ${fmt(chain.checkpoint_height)} confirmed`,'At least two synced sources agree. Mempool measurements belong to each source and are not added together.'];
  }
  function drawChain(){
    if(!chain)return;
    const nodes=chain.nodes||[],indexed=chain.indexed||{},valid=chain.fresh&&chain.quorum,source=nodes.find(n=>n.name===chain.source_node);
    const [status,notice]=chainMessage();$('chain-status').textContent=status;$('overview-chain').textContent=status;$('chain-notice').textContent=notice;
    $('chain-cards').hidden=!valid;
    cards('chain-cards',[['Agreed checkpoint',fmt(chain.checkpoint_height)],['Netspace estimate',space(source?.netspace_bytes)],['Difficulty',fmt(source?.difficulty)],['Indexed blocks',fmt(indexed.blocks)]]);
    const body=$('chain-nodes');body.replaceChildren();
    for(const n of nodes){
      const row=make('tr');
      const state=n.error?'Connection unavailable':!chain.fresh?'Stale':n.synced?'Synced':'Syncing';
      for(const value of [n.name,state,fmt(n.height),space(n.netspace_bytes),fmt(n.mempool_spendbundles),unit(n.mempool_cost==null?null:n.mempool_cost/1e9,'B cost'),stamp(n.observed_at)])row.append(make('td',value));
      if(n.error){const detail=make('small',n.error);row.children[1].append(detail);}body.append(row);
    }
    if(!nodes.length){const row=make('tr'),cell=make('td','No sources configured.');cell.colSpan=7;row.append(cell);body.append(row);}
    const metric=$('chain-metric').value;
    const netspacePower=spacePower(Math.max(1,...(chain.history||[]).filter(p=>p.available&&p.netspace_bytes!=null).map(p=>Number(p.netspace_bytes))));
    const netspaceUnit=spaceUnits[netspacePower];
    const definitions={height:['Block height',v=>v,true,false],netspace_bytes:[`Netspace · ${netspaceUnit}`,v=>v/1024**netspacePower,false,true],mempool_spendbundles:['Mempool spendbundles',v=>v,true,true],mempool_cost:['Mempool · billion CLVM cost',v=>v/1e9,false,true],difficulty:['Difficulty',v=>v,true,false]};
    const [label,transform,integer,zero]=definitions[metric];
    const groups=nodes.map(n=>({name:n.name,points:(chain.history||[]).filter(p=>p.name===n.name).map(p=>({time:p.observed_at,value:p.available&&p[metric]!=null?transform(Number(p[metric])):null}))}));
    $('chain-history-panel').hidden=!groups.some(g=>g.points.some(p=>p.value!=null));
    window.DruidChart.render($('chain-chart'),{groups,gap:180,unit:label,integer,zero,format:v=>`${fmt(v)}${metric==='netspace_bytes'?` ${netspaceUnit}`:metric==='mempool_cost'?' B cost':''}`});chartLegend('chain-legend',groups);
    $('chain-chart-caption').textContent=`${label} by source · last 24 hours of available history. Gaps mean missing measurements.${zero?'':' The vertical axis follows the observed range.'} Hover or use arrow keys for exact values.`;
    $('index-details').hidden=!indexed.blocks;
    cards('index-cards',[['Transaction blocks',fmt(indexed.transaction_blocks)],['Seconds / block',fmt(indexed.seconds_per_block)],['Seconds / transaction block',fmt(indexed.seconds_per_transaction_block)],['Average transaction-block fullness',unit(indexed.average_block_fullness==null?null:indexed.average_block_fullness*100,'%')],['Fee / cost',unit(indexed.fee_per_cost,'mojos')],['Fees in indexed window',unit(indexed.fees_mojos==null?null:Number(indexed.fees_mojos)/1e12,'XCH')],['Coin spends',indexed.blocks_with_unknown_spends?`${fmt(indexed.coin_spends)} · incomplete`:fmt(indexed.coin_spends)],['Reward addresses exceeding 50% combined',fmt(indexed.reward_addresses_over_half)],['Compact blocks · source',valid?fmt(source?.block_counts?.compact):'—'],['Uncompact blocks · source',valid?fmt(source?.block_counts?.uncompact):'—'],['Fee estimate · 60s / 5M cost',valid?unit(source?.fee_estimates_mojos?.[0],'mojos'):'—']]);
    $('index-status').textContent=chain.index_error?`Index paused: ${chain.index_error}`:`Heights ${fmt(indexed.first_height)}–${fmt(indexed.last_height)} · ${fmt(indexed.lag_blocks)} blocks behind checkpoint. ${stamp(indexed.first_tx_timestamp)} to ${stamp(indexed.last_tx_timestamp)}. Payout addresses are not farmer identities or a Nakamoto coefficient. Source block counts observed ${stamp(source?.block_counts_observed_at)}.`;
    ranking('plot-sizes',(indexed.plot_sizes||[]).map(p=>[`k${p.size??'?'} · proof ${p.version??'unreported'}`,p.blocks]),indexed.blocks);
    ranking('reward-addresses',(indexed.top_reward_addresses||[]).map(p=>[p.address||'Unknown',p.blocks]),indexed.blocks);
  }
  function drawVersions(){
    const selected=$('version-choice').value,history=insights?.history||[];
    const points=history.map(p=>({time:p.timestamp,value:(p.versions||[]).find(v=>v.label===selected)?.count??null}));
    const count=points.filter(p=>p.value!=null).length;
    $('version-chart').hidden=count<2;
    const end=Math.max(Date.now()/1000,...points.map(p=>p.time));
    window.DruidChart.render($('version-chart'),{groups:[{name:selected||'Version',points}],start:end-Number($('version-days').value)*86400,end,gap:5400,unit:'Recently connected endpoints',integer:true});
    $('version-chart-caption').textContent=count<2?count===1?`One snapshot recorded for ${selected}. A trend will appear after a second snapshot is collected.`:'No version history in this range. Try a longer time range.':`${count} hourly snapshots for ${selected}. Counts are endpoints, not market share. Missing or unlisted versions remain gaps; only the top 32 versions are retained per snapshot.`;
  }
  function drawInsights(){
    if(!insights)return;const d=insights,geo=d.geography||{};
    $('insights-status').textContent=d.error?'Collection needs attention':`Last ${Math.round((d.window_seconds||300)/60)} minutes`;
    $('insights-status').title=d.error||`Updated ${stamp(d.timestamp)}`;
    cards('quality-cards',[['Completed probes / second',fmt(d.probes_per_second)],['Fresh handshakes · unique endpoints',fmt(d.fresh_endpoints)],['Connection latency · median',unit(d.connect_p50_ms,'ms')],['Connection latency · p95',unit(d.connect_p95_ms,'ms')]]);
    cards('quality-details',[['New endpoints',fmt(d.new_endpoints)],['New gossip endpoints / probe',fmt(d.new_gossip_endpoints_per_probe)],['Peer response latency · p95',unit(d.request_p95_ms,'ms')],['Fresh peer height lag · p95',fmt(d.peer_height_lag_p95)],['Height comparison samples',fmt(d.height_samples)],['Oldest queued job',d.oldest_due_seconds==null?'No jobs due':unit(d.oldest_due_seconds,'seconds')],['Adaptive pacing multiplier',unit(d.adaptive_multiplier,'×')],['Concurrent probes per subnet',fmt(d.subnet_concurrency)],['Reserved slow-peak slots',fmt(d.verification_concurrency)]]);
    const enabled=geo.country_enabled||geo.asn_enabled;
    $('geo-rankings').hidden=!enabled;
    $('geo-status').classList.toggle('notice',!enabled);
    $('geo-status').textContent=enabled?`Country data: ${geo.country_enabled?stamp(geo.country_build_epoch):'not enabled'} · ASN data: ${geo.asn_enabled?stamp(geo.asn_build_epoch):'not enabled'}. Lookups run locally.`:'Geography is not enabled for this crawler. Country and ASN distributions will appear once the databases are installed; unknown locations are not mapped to a country.';
    $('overview-network').textContent=`${fmt((d.distribution?.versions||[]).length)} tracked versions${enabled?' · geography available':' · geography not enabled'}`;
    const total=(d.distribution?.countries||[]).reduce((sum,c)=>sum+c.count,0);
    ranking('countries',(d.distribution?.countries||[]).map(c=>{let name=c.label;try{name=c.label==='unknown'?'Unknown':new Intl.DisplayNames([navigator.language],{type:'region'}).of(c.label);}catch{}return [name,c.count];}),total);
    ranking('asns',(d.distribution?.asns||[]).map(a=>[a.asn==null?'Unknown':`AS${a.asn} · ${a.organization}`,a.count]),total);
    const choice=$('version-choice'),selected=choice.value,counts=new Map();
    for(const snapshot of [...(d.history||[])].sort((a,b)=>a.timestamp-b.timestamp))for(const v of snapshot.versions||[])counts.set(v.label,v.count);
    const versions=[...counts.keys()].sort((a,b)=>counts.get(b)-counts.get(a)||a.localeCompare(b));
    choice.replaceChildren();for(const version of versions){const o=make('option',version);o.value=version;choice.append(o);}if(versions.includes(selected))choice.value=selected;choice.disabled=!versions.length;drawVersions();
  }
  async function refresh(){
    if(busy){pending=true;return;}busy=true;
    await Promise.allSettled([
      load('/api/chain').then(d=>{chain=d;drawChain();}).catch(e=>{$('chain-status').textContent='Telemetry unavailable';$('chain-notice').textContent=`Could not refresh telemetry (${e.message}). Any displayed measurements are from the previous successful update.`;$('overview-chain').textContent='Telemetry unavailable';}),
      load(`/api/insights?days=${$('version-days').value}`).then(d=>{insights=d;drawInsights();}).catch(e=>{$('insights-status').textContent=`Insights unavailable: ${e.message}`;})
    ]);busy=false;if(pending){pending=false;refresh();}
  }
  $('version-days').addEventListener('change',refresh);$('version-choice').addEventListener('change',drawVersions);$('chain-metric').addEventListener('change',drawChain);
  for(const event of ['resize','themechange','viewchange'])window.addEventListener(event,()=>{drawChain();drawVersions();});
  $('index-details').addEventListener('toggle',drawChain);
  refresh();setInterval(()=>{if(!document.hidden)refresh();},60000);
})();
