<script lang="ts">
  import { onMount } from 'svelte'
  import { request } from '../lib/api'
  import type { SharedDiscovery } from '../lib/shared'
  let { discovery, allowWrites }: { discovery: SharedDiscovery; allowWrites: boolean } = $props()
  type Machine = {id: string; name: string; mac: string; broadcast: string; port: number; notes: string}
  type MachineStatus = {id: string; ip?: string; status: string}
  type AdGuard = {enabled: boolean; api_endpoint: string; launch_url: string; username: string; lan_ip: string}
  type AdStatus = {enabled: boolean; protection_enabled: boolean; filtering_enabled: boolean; protection_remaining_seconds?: number; filtering_remaining_seconds?: number}
  type WireGuard = {interface: string; is_running: boolean; writable: boolean; reason: string}
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
  let rewriteDomain = $state('')
  let rewriteAnswer = $state('')
  let wg = $state<WireGuard | null>(null)
  let errors = $state<Record<string,string>>({})
  let busy = $state(false)
  let loading = $state(false)
  let message = $state('')
  let controller: AbortController | undefined
  async function refresh() {
    controller?.abort(); const active = new AbortController(); controller = active
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
    } else await read<WireGuard>('WireGuard', 'wireguard/status', v => wg = v)
    if (!active.signal.aborted) loading = false
  }
  async function mutate(path: string, method: string, body?: unknown, timeout = 60_000): Promise<boolean> {
    if (!allowWrites || busy) return false
    busy = true; errors = {}; message = ''
    try {
      await request('/api/shared/' + path, {method, body: body === undefined ? undefined : JSON.stringify(body)}, timeout)
      message = 'Operation completed'; await refresh(); return true
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
  async function addRewrite() {
    if (await mutate('adguard/rewrites', 'PUT', [...rewrites, {domain: rewriteDomain.trim(), answer: rewriteAnswer.trim()}])) {
      rewriteDomain = ''; rewriteAnswer = ''
    }
  }
  onMount(() => { section = availableTabs[0]?.id || 'wol'; void refresh(); return () => controller?.abort() })
</script>

<div class="connection-tabs">
  {#each availableTabs as tab}<button class:active={section === tab.id} disabled={busy} onclick={() => { section = tab.id; message = ''; password = ''; void refresh() }}>{tab.label}</button>{/each}
  <button disabled={busy || loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
</div>
{#each Object.entries(errors) as [key, error]}<p class="form-error" role="alert">{key}: {error}</p>{/each}
{#if message}<p role="status">{message}</p>{/if}
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
      <label>Notes <input bind:value={machine.notes} /></label><button disabled={busy}>Save machine</button><button type="button" disabled={busy} onclick={() => machine = emptyMachine()}>Reset</button>
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
    <p>Protection: {adStatus.protection_enabled ? 'Enabled' : 'Disabled'} · Filtering: {adStatus.filtering_enabled ? 'Enabled' : 'Disabled'}</p>
    <button disabled={!allowWrites || busy} onclick={() => { if (adStatus && (!adStatus.protection_enabled || confirm('Disable DNS protection?'))) void mutate('adguard/protection', 'POST', {enabled: !adStatus.protection_enabled}) }}>{adStatus.protection_enabled ? 'Disable protection' : 'Enable protection'}</button>
    <button disabled={!allowWrites || busy} onclick={() => { if (adStatus && (!adStatus.filtering_enabled || confirm('Disable DNS filtering?'))) void mutate('adguard/filtering', 'POST', {enabled: !adStatus.filtering_enabled}) }}>{adStatus.filtering_enabled ? 'Disable filtering' : 'Enable filtering'}</button>
  </section>{/if}
  {#if ad?.enabled && !errors['DNS rewrites'] && !loading}<section class="panel"><h2>DNS rewrites</h2><p>{rewrites.length} editable rewrites. Nginx-managed names are excluded.</p>
    <div class="shared-table"><table><thead><tr><th>Domain</th><th>Answer</th><th>Action</th></tr></thead><tbody>
      {#each rewrites as rewrite, index}<tr><td>{rewrite.domain}</td><td>{rewrite.answer}</td><td><button disabled={!allowWrites || busy} onclick={() => { if (confirm('Remove this DNS rewrite?')) void mutate('adguard/rewrites', 'PUT', rewrites.filter((_, i) => i !== index)) }}>Remove</button></td></tr>{/each}
    </tbody></table></div>
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void addRewrite() }}><label>Domain <input bind:value={rewriteDomain} required /></label><label>Answer <input bind:value={rewriteAnswer} required /></label><button disabled={busy}>Add rewrite</button></form>{/if}
  </section>{/if}
{:else}
  <section class="panel"><h2>WireGuard</h2><p>Manage the native wg-quick@wg0.service using the optional restricted admin helper. Configure peers and keys in the root-owned /etc/wireguard/wg0.conf outside Maestro.</p>
    {#if wg}<p><strong>{wg.interface}</strong> · {wg.is_running ? 'Present' : 'Not present'}</p><p class="muted">{wg.reason}</p>
      <button disabled={!allowWrites || busy || !wg.writable || wg.is_running} onclick={() => mutate('wireguard/toggle', 'POST', {enable: true}, 1_600_000)}>Start</button>
      <button disabled={!allowWrites || busy || !wg.writable || !wg.is_running} onclick={() => { if (confirm('Stop WireGuard? This may disconnect remote clients.')) void mutate('wireguard/toggle', 'POST', {enable: false}, 1_600_000) }}>Stop</button>
    {:else if !loading}<p class="muted">Status unavailable.</p>{/if}
  </section>
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
