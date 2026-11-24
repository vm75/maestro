<script lang="ts">
  import { onMount } from 'svelte'
  import { request } from '../lib/api'
  import type { SharedDiscovery } from '../lib/shared'
  let { discovery, allowWrites }: { discovery: SharedDiscovery; allowWrites: boolean } = $props()
  type Machine = {id: string; name: string; mac: string; broadcast: string; port: number; notes: string}
  type MachineStatus = {id: string; ip?: string; status: string}
  type AdGuard = {enabled: boolean; api_endpoint: string; launch_url: string; username: string; lan_ip: string}
  type AdStatus = {enabled: boolean; protection_enabled: boolean; filtering_enabled: boolean; protection_remaining_seconds?: number; filtering_remaining_seconds?: number}
  type WireGuard = {interface: string; is_running: boolean; writable: boolean; peers_writable: boolean; reason: string}
  type Rewrite = {domain: string; answer: string}
  const tabs = [{id: 'wol', label: 'Wake-on-LAN'}, {id: 'adguard', label: 'AdGuard Home'}, {id: 'wireguard', label: 'WireGuard'}]
  const availableTabs = $derived(tabs.filter(t => discovery.modules.some(m => m.id === t.id && m.availability !== 'unsupported')))
  let section = $state('wol')
  let machines = $state<Machine[]>([])
  let machineStatus = $state<MachineStatus[]>([])
  const emptyMachine = () => ({id: '', name: '', mac: '', broadcast: '255.255.255.255', port: 9, notes: ''})
  let machine = $state<Machine>(emptyMachine())
  let ad = $state<AdGuard | null>(null)
  let adStatus = $state<AdStatus | null>(null)
  let password = $state('')
  let rewrites = $state<Rewrite[]>([])
  let rewriteDraft = $state<Rewrite[] | null>(null)
  let pollError = $state('')
  let pollController: AbortController | undefined
  let polling = false
  const pauseMinutes = [1, 10, 30, 60]
  function remaining(seconds?: number) { return seconds == null ? 'Indefinite' : Math.floor(seconds / 60) + 'm ' + (seconds % 60) + 's' }
  function launchAdGuard() {
    try { const url = new URL(ad?.launch_url || ''); if (!['http:', 'https:'].includes(url.protocol)) throw new Error('Invalid launch URL'); window.open(url.href, '_blank', 'noopener,noreferrer') }
    catch { errors['Launch'] = 'Configure a valid HTTP or HTTPS launch URL.' }
  }
  async function pollStatus() {
    if (loading || busy || polling || (section !== 'wol' && section !== 'adguard') || (section === 'adguard' && !ad?.enabled)) return
    const current = section; const active = new AbortController(); pollController = active; polling = true
    try {
      if (current === 'wol') { const value = await request<MachineStatus[]>('/api/shared/wol/status', {signal: active.signal}); if (!active.signal.aborted && section === current && !busy && !loading) machineStatus = value }
      else { const value = await request<AdStatus>('/api/shared/adguard/status', {signal: active.signal}); if (!active.signal.aborted && section === current && !busy && !loading) adStatus = value }
      if (!active.signal.aborted) pollError = ''
    } catch (e) { if (!active.signal.aborted) pollError = String(e) }
    finally { polling = false }
  }
  type Peer = {public_key: string; allowed_ips: string; endpoint: string | null; persistent_keepalive: number | null}
  type PeerConfig = {revision: string; peers: Peer[]; address: string; listen_port: number | null; is_running: boolean}
  let wg = $state<WireGuard | null>(null)
  let peerConfig = $state<PeerConfig | null>(null)
  const emptyPeer = (): Peer => ({public_key: '', allowed_ips: '', endpoint: null, persistent_keepalive: null})
  let peer = $state<Peer>(emptyPeer())
  let editingPeer = $state('')
  let endpoint = $state('')
  let keepalive = $state<number | undefined>(undefined)
  function editPeer(value: Peer) { editingPeer = value.public_key; peer = {...value}; endpoint = value.endpoint || ''; keepalive = value.persistent_keepalive ?? undefined }
  function resetPeer() { editingPeer = ''; peer = emptyPeer(); endpoint = ''; keepalive = undefined }
  async function savePeers(peers: Peer[]) {
    if (!peerConfig || wg?.is_running || peerConfig.is_running) return
    if (await mutate('wireguard/peers', 'PUT', {revision: peerConfig.revision, peers})) resetPeer()
  }
  async function savePeer() {
    if (!peerConfig) return
    const next = {...peer, endpoint: endpoint.trim() || null, persistent_keepalive: keepalive ?? null}
    await savePeers(editingPeer ? peerConfig.peers.map(p => p.public_key === editingPeer ? next : p) : [...peerConfig.peers, next])
  }
  let errors = $state<Record<string,string>>({})
  let busy = $state(false)
  let loading = $state(false)
  let message = $state('')
  let controller: AbortController | undefined
  async function refresh() {
    pollController?.abort(); controller?.abort(); const active = new AbortController(); controller = active
    errors = {}; loading = true
    async function read<T>(key: string, path: string, apply: (value: T) => void) {
      try { const data = await request<T>('/api/shared/' + path, {signal: active.signal}); if (!active.signal.aborted) apply(data) }
      catch (failure) { if (!active.signal.aborted) errors[key] = failure instanceof Error ? failure.message : String(failure) }
    }
    if (section === 'wol') await Promise.all([
      read<Machine[]>('Machines', 'wol', v => machines = v),
      read<MachineStatus[]>('Reachability', 'wol/status', v => machineStatus = v)
    ])
    else if (section === 'adguard') {
      adStatus = null; rewrites = []
      await read<AdGuard>('Configuration', 'adguard/config', v => ad = v)
      if (ad?.enabled && !active.signal.aborted) await Promise.all([
        read<AdStatus>('Status', 'adguard/status', v => adStatus = v),
        read<Rewrite[]>('DNS rewrites', 'adguard/rewrites', v => rewrites = v)
      ])
    } else {
      peerConfig = null
      await read<WireGuard>('WireGuard', 'wireguard/status', v => wg = v)
      if (wg?.peers_writable && !active.signal.aborted) await read<PeerConfig>('Peers', 'wireguard/peers', v => peerConfig = v)
    }
    if (!active.signal.aborted) loading = false
  }
  async function mutate(path: string, method: string, body?: unknown, timeout = 60_000): Promise<boolean> {
    if (!allowWrites || busy) return false
    busy = true; errors = {}; message = ''
    try {
      pollController?.abort()
      const result = await request<{message?: string; machine?: string; packets_sent?: number; simulated?: boolean}>('/api/shared/' + path, {method, body: body === undefined ? undefined : JSON.stringify(body)}, timeout)
      message = result.packets_sent !== undefined ? `${result.simulated ? 'Simulated' : 'Sent'} ${result.packets_sent} packets to ${result.machine}` : result.message || 'Operation completed'; await refresh(); return true
    } catch (failure) { errors['Operation'] = failure instanceof Error ? failure.message : String(failure); return false }
    finally { busy = false }
  }
  async function saveMachine() {
    const {id, ...body} = machine
    if (await mutate('wol' + (id ? '/' + encodeURIComponent(id) : ''), id ? 'PUT' : 'POST', body)) machine = emptyMachine()
  }
  async function saveAdGuard() {
    if (!ad) return
    try { await mutate('adguard/config', 'PUT', {...ad, ...(password ? {password} : {})}) }
    finally { password = '' }
  }
  async function saveRewrites() { if (rewriteDraft && await mutate('adguard/rewrites', 'PUT', rewriteDraft)) rewriteDraft = null }
  function switchTab(id: string) { if (rewriteDraft && !confirm('Discard unsaved DNS rewrite edits?')) return; rewriteDraft = null; section = id; message = ''; password = ''; pollError = ''; void refresh() }
  onMount(() => { section = availableTabs[0]?.id || 'wol'; void refresh(); const timer = setInterval(pollStatus, 3000); return () => { clearInterval(timer); controller?.abort(); pollController?.abort() } })
</script>

<div class="connection-tabs">
  {#each availableTabs as tab}<button class:active={section === tab.id} disabled={busy} onclick={() => switchTab(tab.id)}>{tab.label}</button>{/each}
  <button disabled={busy || loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
</div>
{#each Object.entries(errors) as [key, error]}<p class="form-error" role="alert">{key}: {error}</p>{/each}
{#if message}<p role="status">{message}</p>{/if}
{#if pollError}<p class="form-error" role="alert">Status refresh: {pollError} (showing last known status)</p>{/if}
{#if section === 'wol'}
  <section class="panel"><h2>Wake-on-LAN machines</h2><p>Send magic packets from this server to machines on a reachable LAN.</p>
    <div class="shared-table"><table><thead><tr><th>Machine</th><th>MAC address</th><th>Reachability</th><th>Actions</th></tr></thead><tbody>
      {#each machines as item (item.id)}
        {@const reach = machineStatus.find(s => s.id === item.id)}
        <tr><td>{item.name}<small>{item.notes}</small></td><td class="mono">{item.mac}</td><td>{reach?.status || 'unknown'} {reach?.ip || ''}</td><td>
          <button disabled={!allowWrites || busy} onclick={() => mutate('wol/' + item.id + '/wake', 'POST')}>Wake</button>
          <button disabled={!allowWrites || busy} onclick={() => machine = {...item}}>Edit</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Delete ' + item.name + '?')) void mutate('wol/' + item.id, 'DELETE') }}>Delete</button>
        </td></tr>
      {/each}
    </tbody></table></div>
    {#if !machines.length && !loading}<p class="muted">No machines configured.</p>{/if}
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void saveMachine() }}>
      <h3>{machine.id ? 'Edit machine' : 'Add machine'}</h3>
      <label>Name <input bind:value={machine.name} required /></label><label>MAC address <input bind:value={machine.mac} placeholder="aa:bb:cc:dd:ee:ff" required /></label>
      <label>Broadcast address <input bind:value={machine.broadcast} required /></label><label>UDP port <input type="number" min="1" max="65535" bind:value={machine.port} required /></label>
      <label>Notes <textarea bind:value={machine.notes} rows="3"></textarea></label><button disabled={busy}>Save machine</button><button type="button" disabled={busy} onclick={() => machine = emptyMachine()}>Reset</button>
    </form>{/if}
  </section>
{:else if section === 'adguard'}
  <section class="panel"><h2>AdGuard Home connection</h2><p>Connect to an existing AdGuard Home instance; Maestro does not install the daemon.</p>
    {#if ad}<form onsubmit={e => { e.preventDefault(); void saveAdGuard() }}>
      <fieldset disabled={!allowWrites || busy}>
        <label><input type="checkbox" bind:checked={ad.enabled} /> Enable integration</label>
        <label>API endpoint <input type="url" bind:value={ad.api_endpoint} placeholder="http://127.0.0.1:3000" required={ad.enabled} /></label>
        <label>Launch URL <input type="url" bind:value={ad.launch_url} /></label>
        <label>Username <input bind:value={ad.username} /></label>
        <label>Password (leave blank to retain) <input type="password" autocomplete="new-password" bind:value={password} /></label>
        <label>LAN IP <input bind:value={ad.lan_ip} /></label>
        <button>Save connection</button>
      </fieldset>
    </form>{/if}
  </section>
  {#if adStatus?.enabled}<section class="panel"><h2>Protection and filtering</h2>
    <button type="button" onclick={launchAdGuard}>Launch Web UI</button>
    {#each ['protection', 'filtering'] as kind}
      {@const enabled = kind === 'protection' ? adStatus.protection_enabled : adStatus.filtering_enabled}
      {@const seconds = kind === 'protection' ? adStatus.protection_remaining_seconds : adStatus.filtering_remaining_seconds}
      <h3>DNS {kind}</h3><p>{enabled ? 'Enabled' : 'Paused · ' + remaining(seconds)}</p>
      {#if enabled}
        {#each pauseMinutes as duration}
          <button disabled={!allowWrites || busy} onclick={() => mutate('adguard/' + kind, 'POST', {enabled: false, duration_minutes: duration})}>{duration} min</button>
        {/each}
      {:else}<button disabled={!allowWrites || busy} onclick={() => mutate('adguard/' + kind, 'POST', {enabled: true})}>Resume</button>{/if}
    {/each}
  </section>{/if}
  {#if ad?.enabled && !errors['DNS rewrites'] && !loading}<section class="panel"><h2>DNS rewrites</h2><p>{rewrites.length} editable rewrites. Nginx-managed names are excluded.</p>
    <div class="shared-table"><table><thead><tr><th>Domain</th><th>Answer</th><th>Action</th></tr></thead><tbody>
      {#each rewrites as rewrite, index}<tr><td>{rewrite.domain}</td><td>{rewrite.answer}</td><td><button disabled={!allowWrites || busy || !!rewriteDraft} onclick={() => { if (confirm('Remove this DNS rewrite?')) void mutate('adguard/rewrites', 'PUT', rewrites.filter((_, i) => i !== index)) }}>Remove</button></td></tr>{/each}
    </tbody></table></div>
    {#if allowWrites}
      {#if rewriteDraft}
        <form onsubmit={e => { e.preventDefault(); void saveRewrites() }}>
          {#each rewriteDraft as rewrite, index}
            <div class="editor-grid"><label>Domain <input bind:value={rewrite.domain} required /></label><label>Answer <input bind:value={rewrite.answer} required /></label><button type="button" disabled={busy} onclick={() => { if (rewriteDraft) rewriteDraft = rewriteDraft.filter((_, i) => i !== index) }}>Delete</button></div>
          {/each}
          <button type="button" disabled={busy} onclick={() => { if (rewriteDraft) rewriteDraft = [...rewriteDraft, {domain: '', answer: ''}] }}>Add rewrite</button><button disabled={busy}>Save rewrites</button><button type="button" disabled={busy} onclick={() => rewriteDraft = null}>Cancel</button>
        </form>
      {:else}<button disabled={busy} onclick={() => rewriteDraft = rewrites.map(r => ({...r}))}>Manage DNS rewrites</button>{/if}
    {/if}
  </section>{/if}
{:else}
  <section class="panel"><h2>WireGuard</h2><p>Manage the native wg-quick@wg0.service using the optional restricted admin helper. Peer editing requires the separate wireguard_peers grant. Interface private keys, hooks and addresses remain administrator-owned.</p>
    {#if wg}<p><strong>{wg.interface}</strong> · {wg.is_running ? 'Present' : 'Not present'}</p><p class="muted">{wg.reason}</p>
      <button disabled={!allowWrites || busy || !wg.writable || wg.is_running} onclick={() => mutate('wireguard/toggle', 'POST', {enable: true}, 1_600_000)}>Start</button>
      <button disabled={!allowWrites || busy || !wg.writable || !wg.is_running} onclick={() => { if (confirm('Stop WireGuard? This may disconnect remote clients.')) void mutate('wireguard/toggle', 'POST', {enable: false}, 1_600_000) }}>Stop</button>
    {:else if !loading}<p class="muted">Status unavailable.</p>{/if}
  </section>
  {#if peerConfig}
    <section class="panel"><h2>WireGuard peers</h2>
      <p>Interface address: {peerConfig.address || '—'} · Listen port: {peerConfig.listen_port ?? '—'}</p>
      <p class="policy-note">Stop the interface before editing peers, then start it again so wg-quick applies routes. Existing preshared keys are retained when the public key is unchanged. Private keys are never returned.</p>
      <div class="shared-table"><table><thead><tr><th>Public key</th><th>Allowed IPs</th><th>Endpoint</th><th>Keepalive</th><th>Actions</th></tr></thead><tbody>
        {#each peerConfig.peers as item (item.public_key)}
          <tr><td class="mono">{item.public_key}</td><td>{item.allowed_ips}</td><td>{item.endpoint || '—'}</td><td>{item.persistent_keepalive ?? '—'}</td><td>
            <button disabled={!allowWrites || busy || wg?.is_running || peerConfig.is_running} onclick={() => editPeer(item)}>Edit</button>
            <button disabled={!allowWrites || busy || wg?.is_running || peerConfig.is_running} onclick={() => { if (peerConfig && confirm('Remove this WireGuard peer?')) void savePeers(peerConfig.peers.filter(p => p.public_key !== item.public_key)) }}>Remove</button>
          </td></tr>
        {/each}
      </tbody></table></div>
      {#if allowWrites}<form onsubmit={event => { event.preventDefault(); void savePeer() }}>
        <fieldset disabled={busy || wg?.is_running || peerConfig.is_running}>
          <h3>{editingPeer ? 'Edit peer' : 'Add peer'}</h3>
          <label>Public key <input bind:value={peer.public_key} required disabled={!!editingPeer} /></label>
          <label>Allowed IP networks (comma separated) <input bind:value={peer.allowed_ips} required /></label>
          <label>Endpoint (optional) <input bind:value={endpoint} placeholder="vpn.example.net:51820" /></label>
          <label>Persistent keepalive (seconds) <input type="number" min="0" max="65535" bind:value={keepalive} /></label>
          <button>Save peer</button><button type="button" onclick={resetPeer}>Reset</button>
        </fieldset>
      </form>{/if}
    </section>
  {:else if wg && !wg.peers_writable}<p class="policy-note">To enable peer management, authorize wireguard_peers in the root-owned helper policy.</p>{/if}
{/if}

<style>
  .shared-table { overflow-x: auto; margin-top: 1rem; }
  table { width: 100%; border-collapse: collapse; text-align: left; }
  th, td { padding: .7rem; border-bottom: 1px solid var(--border); }
  th { color: var(--muted); font-size: .78rem; }
  td small { display: block; color: var(--muted); }
  form, fieldset { display: grid; gap: .75rem; }
  fieldset { border: 0; padding: 0; }
</style>
