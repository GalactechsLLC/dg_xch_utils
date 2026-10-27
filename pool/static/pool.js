"use strict";
const root = document.getElementById("content");
const el = (tag, text, cls) => { const node=document.createElement(tag); if(text!==undefined)node.textContent=text; if(cls)node.className=cls; return node; };
const link = (text, href) => {const node=el("a",text);node.href=href;return node;};
const count = value => value == null ? "—" : BigInt(value).toLocaleString();
const short = value => value ? `${value.slice(0,10)}…${value.slice(-8)}` : "—";
const when = value => value ? new Date(Number(value)*1000).toLocaleString() : "No observations";
const amount = value => {const n=BigInt(value); return `${(n/1000000000000n).toLocaleString()}.${(n%1000000000000n).toString().padStart(12,"0").replace(/0+$/,"")||"0"}`;};
const path = location.pathname;
let session=null, info=null, generation=0;
let params=new URLSearchParams(location.search);
let tab=params.get("tab")||"statistics";
let ticket=new URLSearchParams(location.hash.slice(1)).get("ticket");
if(location.hash) history.replaceState(null,"",location.pathname+location.search);
async function read(url, options={}) {
  const current=generation;
  const response=await fetch(url,{cache:"no-store",credentials:"same-origin",signal:AbortSignal.timeout(15000),...options});
  const data=await response.json();
  if(current!==generation)throw new DOMException("View changed", "AbortError");
  if(!response.ok || data.error || data.error_code) {const error=new Error(data.error||data.error_message||"Pool request failed");error.status=response.status;throw error;}
  return data;
}
function post(url,body) {return read(url,{method:"POST",headers:{"Content-Type":"application/json",...(session?{"X-CSRF-Token":session.csrf}:{})},body:JSON.stringify(body)});}
function heading(title,subtitle) {document.getElementById("title").textContent=title;document.getElementById("subtitle").textContent=subtitle;document.title=`${title} · Druid Garden`;}
function panel(title) {const section=el("section",undefined,"panel");if(title)section.append(el("h2",title));root.append(section);return section;}
function note(parent,text) {parent.append(el("p",text,"muted"));}
function code(parent,text) {const pre=el("pre");pre.append(el("code",text));parent.append(pre);}
function stats(items) {const grid=el("section",undefined,"stats-grid");for(const [label,value,help] of items){const card=el("article",undefined,"stat");card.append(el("span",label),el("strong",value),el("small",help));grid.append(card);}root.append(grid);}
function button(text,action) {const b=el("button",text);b.type="button";b.addEventListener("click",async()=>{b.disabled=true;try{await action();}catch(error){document.getElementById("status").textContent=error.message;}finally{b.disabled=false;}});return b;}
function filters({search=false,sort=false,status=false}={}) {
  const form=el("form",undefined,"filters");
  const select=(label,key,values)=>{const wrap=el("label",label);const input=el("select");for(const [value,title] of values){const option=el("option",title);option.value=value;input.append(option);}input.value=params.get(key)||values[0][0];input.name=key;wrap.append(input);form.append(wrap);};
  select("Period","days",[["1","24 hours"],["7","7 days"],["30","30 days"],["90","90 days"],["365","1 year"]]);
  if(search){const label=el("label","Farmer name or launcher ID");const input=el("input");input.name="search";input.maxLength=128;input.value=params.get("search")||"";label.append(input);form.append(label);}
  if(sort)select("Rank by","sort",[["points","Verified points"],["blocks","Blocks claimed"],["recent","Latest partial"]]);
  if(status){select("Partial status","status",[["accepted","Accepted"],["duplicate","Duplicate"],["invalid","Invalid"]]);const label=el("label","Harvester ID (optional)");const input=el("input");input.name="harvester";input.maxLength=66;input.value=params.get("harvester")||"";label.append(input);form.append(label);}
  if(path==="/account"&&session){select("PlotNFT","launcher",[["","All linked PlotNFTs"],...session.farmers.map(f=>[f.launcher_id,f.name||short(f.launcher_id)])]);}
  const apply=el("button","Apply");apply.type="submit";form.append(apply);
  form.addEventListener("submit",event=>{event.preventDefault();params=new URLSearchParams(new FormData(form));if(path==="/account")params.set("tab",tab);history.replaceState(null,"",`${path}?${params}`);render();});root.append(form);
}
function query() {const q=new URLSearchParams(params);q.delete("tab");return q;}
function table(parent,columns,rows) {
  if(!rows.length){note(parent,"No records in this period yet.");return;}
  const scroll=el("div",undefined,"table-scroll");const table=el("table");const head=el("thead");const tr=el("tr");for(const [label] of columns){const th=el("th",label);th.scope="col";tr.append(th);}head.append(tr);table.append(head);const body=el("tbody");
  for(const row of rows){const tr=el("tr");for(const [,format] of columns){const td=el("td");const value=format(row);td.append(value instanceof Node?value:document.createTextNode(String(value??"—")));tr.append(td);}body.append(tr);}table.append(body);scroll.append(table);parent.append(scroll);
}
function hash(value) {const node=el("span",short(value),"hash");node.title=value||"";return node;}
function pager(parent,data) {const nav=el("nav",undefined,"pager");nav.setAttribute("aria-label","Pagination");const move=number=>{params.set("page",String(number));history.replaceState(null,"",`${path}?${params}`);return render();};if(data.page>0)nav.append(button("← Previous",()=>move(data.page-1)));nav.append(el("span",`Page ${data.page+1}`));if(data.has_more)nav.append(button("Next →",()=>move(data.page+1)));parent.append(nav);}
function chart(parent,data) {
  if(!data.history.length){note(parent,"No accepted partials in this period yet.");return;}
  const ns="http://www.w3.org/2000/svg";const svg=document.createElementNS(ns,"svg");svg.setAttribute("viewBox","0 0 900 210");svg.setAttribute("role","img");svg.setAttribute("aria-label","Accepted partials over time; each bar includes its time and count");svg.classList.add("activity-chart");
  const interval=Number(params.get("days")||1)<=1?3600:86400;const start=Math.floor(data.since/interval)*interval;const end=Math.floor(data.now/interval)*interval;const buckets=new Map(data.history.map(item=>[item.time,item]));const n=(end-start)/interval+1;const max=Math.max(...data.history.map(item=>item.partials),1);
  for(let i=0;i<n;i++){const time=start+i*interval;const item=buckets.get(time)||{partials:0,points:"0"};const rect=document.createElementNS(ns,"rect");const height=item.partials/max*170;rect.setAttribute("x",String(i*900/n));rect.setAttribute("y",String(180-height));rect.setAttribute("width",String(Math.max(1,900/n-2)));rect.setAttribute("height",String(Math.max(height,1)));const title=document.createElementNS(ns,"title");title.textContent=`${when(time)}: ${count(item.partials)} partials, ${count(item.points)} points`;rect.setAttribute("tabindex","0");rect.setAttribute("aria-label",title.textContent);rect.append(title);svg.append(rect);}
  parent.append(svg);const labels=el("div",undefined,"status-line");labels.append(el("small",when(start)),el("small",when(end)));parent.append(labels);note(parent,"Accepted partials per time bucket. Hover a bar for its count; missing buckets have no recorded accepted partials.");
}
function exportCsv(parent,rows) {
  parent.append(button("Download this page as CSV",()=>{if(!rows.length)return;const keys=Object.keys(rows[0]).filter(key=>key!=="allocations");const cell=value=>`"${String(value??"").replace(/^[=+@-]/,"'$&").replaceAll('"','""')}"`;const text=[keys,...rows.map(row=>keys.map(key=>row[key]))].map(row=>row.map(cell).join(",")).join("\r\n");const url=URL.createObjectURL(new Blob([text],{type:"text/csv;charset=utf-8"}));const anchor=link("",url);anchor.download="druid-pool-history.csv";anchor.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}));
}
async function listing(kind,privatePage=false) {
  const data=await read(`/api/${privatePage?"account/":""}${kind}?${query()}`);const section=panel(kind[0].toUpperCase()+kind.slice(1));
  const columns={
    farmers:[["Farmer",r=>r.name||hash(r.launcher_id)],["Verified points",r=>count(r.points)],["Accepted partials",r=>count(r.partials)],["Blocks claimed",r=>count(r.blocks)],["Last accepted",r=>when(r.last_seen)]],
    leaderboards:[["Farmer",r=>r.name||hash(r.launcher_id)],["Verified points",r=>count(r.points)],["Blocks claimed",r=>count(r.blocks)],["Last accepted",r=>when(r.last_seen)]],
    blocks:[["Recorded",r=>when(r.time)],["Farmer",r=>hash(r.launcher_id)],["Reward coin",r=>hash(r.reward_coin)],[`Reward (${info.currency_code})`,r=>amount(r.amount)]],
    payouts:[["Round",r=>when(r.time)],["Reward coin",r=>hash(r.reward_coin)],[privatePage?`Your allocation (${info.currency_code})`:`Distributed (${info.currency_code})`,r=>amount(r.amount)],["Status",r=>r.confirmed_height==null?"Awaiting confirmation":`Confirmed · ${count(r.confirmed_height)}`]],
    partials:[["Received",r=>when(r.time)],["PlotNFT",r=>hash(r.launcher_id)],["Partial",r=>hash(String(r.id))],["Status",r=>r.status],["Points",r=>r.points==null?"—":count(r.points)]],
    harvesters:[["Harvester",r=>hash(r.harvester)],["PlotNFT",r=>hash(r.launcher_id)],["Requests",r=>count(r.requests)],["Accepted",r=>count(r.accepted)],["Last seen",r=>when(r.last_seen)],["Pool verification",r=>`${Math.round(r.verification_ms)} ms`]],
  }[kind];table(section,columns,data.items);pager(section,data);
  if(privatePage)exportCsv(section,data.items);
  if(kind==="harvesters")note(section,"Harvester observations begin when diagnostics are enabled. Timing measures pool verification, not disk proof recovery.");
  if(kind==="blocks")note(section,"These are recorded reward claims; timestamps show when the pool recorded each claim.");
  if(privatePage&&kind==="payouts") {const addresses=panel("Payout destinations in these rounds");table(addresses,[["PlotNFT",r=>hash(r.launcher_id)],["Historical address",r=>hash(r.address)],[`Allocation (${info.currency_code})`,r=>amount(r.amount)]],data.items.flatMap(item=>item.allocations));}
}
function loginHelp(parent) {
  note(parent,"Generate a login link on the device that holds your PlotNFT owner key. Your keys stay on that device. Open the link within two minutes.");
  code(parent,"dgx farmer pool-login --config /path/to/farmer.yaml");
  note(parent,"If the configuration has multiple PlotNFTs, add --launcher-id <LAUNCHER_ID>. Login requires a registered PlotNFT in this pool.");
}
async function accountPage() {
  try{session=await read("/api/account");}catch(error){if(error.status!==401)throw error;heading("Your farm, in one place.","Sign in with your PlotNFT to see your private farming history.");loginHelp(panel("PlotNFT login"));return;}
  heading("My farm","Your linked PlotNFTs, contributions and rewards.");
  const tabs=el("nav",undefined,"tabs");tabs.setAttribute("aria-label","Farmer pages");for(const [value,label]of [["statistics","Overview & statistics"],["partials","Partials"],["harvesters","Harvesters"],["blocks","Blocks"],["payouts","Payouts"],["settings","Account settings"]]){const a=link(label,`/account?tab=${value}`);if(tab===value)a.setAttribute("aria-current","page");tabs.append(a);}root.append(tabs);
  if(tab==="settings") {
    const identities=panel("Linked PlotNFTs & payout addresses");
    for(const farmer of session.farmers){const card=el("article",undefined,"identity");card.append(el("h3",farmer.name||short(farmer.launcher_id)));const dl=el("dl");for(const [label,value]of [["Launcher ID",farmer.launcher_id],["Pool contract",farmer.contract_puzzle_hash],["Payout puzzle hash",farmer.payout_puzzle_hash]]){const row=el("div");row.append(el("dt",label),el("dd",value,"hash"));dl.append(row);}card.append(dl);const form=el("form",undefined,"filters");const label=el("label","Public farmer name");const input=el("input");input.value=farmer.name;input.maxLength=64;label.append(input);form.append(label);const save=el("button","Save name");save.type="submit";form.append(save);form.addEventListener("submit",async event=>{event.preventDefault();save.disabled=true;try{await post("/api/account/name",{launcher_id:farmer.launcher_id,name:input.value});document.getElementById("status").textContent="Farmer name saved";}catch(error){document.getElementById("status").textContent=error.message;}finally{save.disabled=false;}});card.append(form);identities.append(card);}
    const linking=panel("Link another farm");note(linking,"Prove ownership of both PlotNFTs using their existing farmer configurations. Linking combines their account views, including multiple payout addresses. It does not change reward destinations. Existing browser sessions are revoked.");code(linking,"dgx farmer pool-login --config /path/to/first.yaml --link-config /path/to/second.yaml");note(linking,"To separate one PlotNFT and revoke the linked account’s sessions:");code(linking,"dgx farmer pool-login --config /path/to/farmer.yaml --launcher-id <LAUNCHER_ID> --unlink");linking.append(button("Sign out",async()=>{await post("/api/logout",{});session=null;location.assign("/login");}));return;
  }
  filters({status:tab==="partials"});
  if(tab==="statistics") {const data=await read(`/api/account/statistics?${query()}`);const total=data.history.reduce((n,item)=>n+BigInt(item.points),0n);stats([["Linked PlotNFTs",count(session.farmers.length),"Independently verified ownership"],["Accepted partials",count(data.history.reduce((n,item)=>n+item.partials,0)),"Selected period"],["Verified points",count(total),"Selected period"],["Payout addresses",count(new Set(session.farmers.map(f=>f.payout_puzzle_hash)).size),"Current registered destinations"]]);chart(panel("Your contribution history"),data);const activity=panel("Farm activity");note(activity,`Last accepted partial: ${when(data.last_partial)}. Active PlotNFTs in this period: ${count(data.active_farmers)}.`);if(!data.last_partial||data.now-data.last_partial>3600)note(activity,"No accepted partial in the last hour. Small farms may naturally submit infrequently; check your farmer connection if this is unexpected.");table(activity,[["Observed status",r=>r.status],["Requests",r=>count(r.count)]],data.diagnostics);note(activity,"Request diagnostics start when monitoring is enabled; credited partial history is retained separately.");}
  else if(["partials","harvesters","blocks","payouts"].includes(tab))await listing(tab,true);
  else throw new Error("Unknown farmer page");
}
async function render() {
  const current=++generation;root.replaceChildren();root.setAttribute("aria-busy","true");
  try{
    info=info||await read("/pool_info");
    if(path==="/account")await accountPage();
    else if(path==="/login") {
      heading("Welcome back.","Your PlotNFT is your identity.");const section=panel("PlotNFT login");
      if(ticket){note(section,"A login link is ready. Continue to open the account proven by this PlotNFT.");section.append(button("Continue to my farm",async()=>{session=await post("/api/session",{ticket});ticket=null;location.assign("/account");}));}else loginHelp(section);
    } else if(path==="/") {
      heading(info.name||"Grow together.",info.description||"A shared garden. Every contribution accounted for.");filters();const data=await read(`/api/overview?${query()}`);stats([["Farmers",count(data.totals.farmers),"Registered PlotNFTs"],["Accepted partials",count(data.totals.accepted_partials),"Retained accounting history"],["Reward claims",count(data.totals.reward_claims),"Recorded pool rewards"],["Confirmed payouts",count(data.totals.confirmed_payouts),`${count(data.totals.signed_payouts)} signed batches`]]);chart(panel("Pool contribution history"),data);const next=panel("Room to grow");note(next,"Track your contributions, connect your PlotNFTs and follow every reward round.");next.append(link("Open my farm →","/account"),document.createTextNode(" · "),link("Start pooling →","/join"));
    } else if(["/farmers","/leaderboards","/blocks","/payouts"].includes(path)) {
      const kind=path.slice(1);heading({farmers:"Meet the growers.",leaderboards:"The garden’s contributors.",blocks:"Every reward, recorded.",payouts:"Follow the rewards."}[kind],"Recorded contributions and rewards from this pool.");filters({search:kind==="farmers"||kind==="leaderboards",sort:kind==="leaderboards"});await listing(kind);
    } else if(path==="/pool-status") {
      heading("A pulse on the garden.","Pool storage and recorded farming activity.");const data=await read("/api/overview?days=1");const section=panel("Pool service");note(section,`Database readable · ${info.chain_profile} · protocol ${info.protocol_version}`);note(section,`Last accepted partial: ${when(data.last_partial)}. Active farmers in 24 hours: ${count(data.active_farmers)}.`);table(section,[["Partial status",r=>r.status],["Observed requests",r=>count(r.count)]],data.diagnostics);note(section,"This page reports pool observations. Database availability alone does not establish full-node synchronization or peer connectivity.");stats([["Farmers",count(data.totals.farmers),"Registered"],["Partials",count(data.totals.accepted_partials),"Accepted"],["Signed payouts",count(data.totals.signed_payouts),"Recorded batches"],["Confirmed payouts",count(data.totals.confirmed_payouts),"Recorded confirmations"]]);chart(panel("Last 24 hours"),data);
    } else if(path==="/join") {
      heading("Plant your farm here.","Pool with your existing PlotNFT and keep control of your keys.");const section=panel("Connect to the pool");note(section,"Use a DGX farmer configured for this pool’s chain. Create or join a PlotNFT using your existing owner key, then add the pool URL and PlotNFT to your farmer configuration.");code(section,location.origin);note(section,`Pool target: ${info.target_puzzle_hash}`);note(section,"Portable plots must be created for that PlotNFT’s pool contract. Solo plots cannot submit pool partials. Keep the original owner key and farmer configuration backed up.");note(section,"After the farmer registers and submits accepted partials, use its configuration to open your private dashboard.");loginHelp(section);
    } else if(path==="/fees") {
      heading("Clear contributions. Clear rewards.","How this pool accounts for rewards.");const section=panel("Fees & distribution");note(section,`Configured pool fee: ${(Number(info.fee)*100).toLocaleString()}%. Minimum partial difficulty: ${info.minimum_difficulty}. Relative lock height: ${info.relative_lock_height} blocks.`);note(section,"Rewards use the verified points in each recorded reward round. Integer allocation preserves every mojo; shared payout addresses are grouped after each PlotNFT’s share is calculated. Transaction fees, when configured, are deducted before distributing the remainder.");note(section,"Signed payouts remain pending until the node confirms them. Your account shows your individual historical allocation, even if multiple PlotNFTs share a payout address.");
    } else if(path==="/api-help") {
      heading("Build with the garden.","Read pool observations or authenticate your farmer.");const section=panel("Public API");for(const endpoint of ["/pool_info","/pool_stats","/api/overview","/api/farmers","/api/leaderboards","/api/blocks","/api/payouts"]){const p=el("p");p.append(link(endpoint,endpoint));section.append(p);}note(section,`List endpoints use page=0,1,… with up to 50 rows and has_more. Period filters accept days=1,7,30,90,365. Monetary amounts are decimal strings in mojos; 1 ${info.currency_code} = 1,000,000,000,000 mojos.`);note(section,"Private /api/account/* endpoints require a PlotNFT browser session. Generate signed login proofs with the DGX client and exchange them at /web/login. The client selects the signing protocol for your chain; browser sessions and account pages are shared across profiles.");
    } else throw new Error("Unknown pool page");
    if(current===generation){document.getElementById("status").textContent="Pool records available";document.getElementById("updated").textContent=`Updated ${new Date().toLocaleTimeString()}`;}
  }catch(error){if(current!==generation)return;document.getElementById("status").textContent=error.message;const section=panel("Unable to load this view");note(section,"Your connection may have changed or your login may have expired. Retry, or generate a fresh PlotNFT login link.");section.append(button("Retry",render));}
  finally{if(current===generation)root.setAttribute("aria-busy","false");}
}
for(const anchor of document.querySelectorAll(".pool-nav a"))if(anchor.pathname===path)anchor.setAttribute("aria-current","page");
render();
// Do not replace forms, selected text, or a one-use login prompt during editing.
setInterval(()=>{if(!document.hidden&&["/","/pool-status"].includes(path)&&!root.contains(document.activeElement))render();},60000);
