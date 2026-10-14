'use strict';
const element = id => document.getElementById(id);
const count = value => Number(value).toLocaleString();
const text = (id, value) => { element(id).textContent = value; };
function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor(seconds % 86400 / 3600);
  const minutes = Math.floor(seconds % 3600 / 60);
  return days ? `${days}d ${hours}h` : hours ? `${hours}h ${minutes}m` : minutes ? `${minutes}m` : `${Math.floor(seconds)}s`;
}
let busy = false;
async function refresh() {
  if (busy) return;
  busy = true;
  try {
    const response = await fetch('/api/stats', {cache: 'no-store', signal: AbortSignal.timeout(10000)});
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const s = await response.json();
    text('status', `${s.network} · service running · ${count(s.verified_peers)} verified peers`);
    element('status').parentElement.classList.remove('error');
    text('updated', `Updated ${new Date(s.timestamp * 1000).toLocaleString()}`);
    text('peers', count(s.known_peers));
    text('families', `${count(s.verified_peers)} verified · ${count(s.ipv4_peers)} IPv4 · ${count(s.ipv6_peers)} IPv6`);
    text('requests', count(s.peer_requests));
    text('shared', count(s.peers_sent));
    text('uptime', duration(s.uptime_seconds));
    text('network', `Network: ${s.network}`);
    text('active', `${count(s.active_connections)} / ${count(s.connection_capacity)}`);
    text('probes', `${count(s.probe_attempts)} total · ${count(s.active_probes)} active`);
    text('probe-success', count(s.probe_successes));
    text('probe-failures', `${count(s.probe_errors)} failed · ${count(s.probe_timeouts)} timed out`);
    text('rejections', `${count(s.rejected_capacity)} capacity · ${count(s.rejected_duplicate_ip)} duplicate IP`);
    text('cooldowns', count(s.probe_cooldowns));
    text('ttl', duration(s.peer_ttl_seconds));
    text('pending', count(s.pending_verification));
    for (const [id, key] of [['connections','connections'], ['handshakes','handshakes'], ['responses','peer_responses'], ['registrations','registrations'], ['expired','expired_peers'], ['session-errors','session_errors'], ['session-timeouts','session_timeouts']]) text(id, count(s[key]));
  } catch (error) {
    text('status', `Update failed · ${error.message}. Retrying in 15 seconds.`);
    element('status').parentElement.classList.add('error');
  } finally { busy = false; }
}
refresh();
setInterval(refresh, 15000);
