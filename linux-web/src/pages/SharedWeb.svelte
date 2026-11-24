<script lang="ts">
  import { onMount } from 'svelte'
  import Icon from '../lib/Icon.svelte'
  import { request } from '../lib/api'
  import type { SharedDiscovery } from '../lib/shared'

  let { discovery }: { discovery: SharedDiscovery } = $props()
  const hasNginx = $derived(discovery.modules.some(m => m.id === 'nginx' && m.availability === 'ready'))
  const hasCertificates = $derived(discovery.modules.some(m => m.id === 'certificates' && m.availability === 'ready'))
  let section = $state<'nginx' | 'certificates'>('nginx')
  interface NginxStatus { running: boolean; config_path: string; root_dir: string }
  interface Site { kind: string; domain: string; name: string; display_name: string; port?: number; managed: boolean; enabled: boolean; state: string }
  interface Log { path: string; size: number }
  interface Settings { listener_profiles: number; access_policies: number; limit_policies: number; allowed_ips: number }
  interface Certificate { id: string; name: string; domains: string[]; method: string; staging: boolean; auto_renew: boolean; exists: boolean; expires_at?: string; days_remaining?: number; renewal_due: boolean }
  interface HttpAccess { state: string; expires_at?: string; names: string[]; wan_control: string }
  interface Lock { locked: boolean; status: string }
  let status = $state<NginxStatus | null>(null)
  let sites = $state<Site[]>([])
  let settings = $state<Settings | null>(null)
  let logs = $state<Log[]>([])
  let certificates = $state<Certificate[]>([])
  let httpAccess = $state<HttpAccess | null>(null)
  let lock = $state<Lock | null>(null)
  let errors = $state<Record<string, string>>({})
  let loading = $state(false)
  let filter = $state('')
  let selectedLog = $state('')
  let logText = $state('')
  let logError = $state('')
  let logLoading = $state(false)
  let controller: AbortController | undefined
  let logController: AbortController | undefined
  const visibleSites = $derived(sites.filter(site => (site.display_name + ' ' + site.kind + ' ' + site.domain).toLowerCase().includes(filter.toLowerCase())))

  async function refresh() {
    controller?.abort()
    const active = new AbortController(); controller = active
    loading = true; errors = {}
    async function read<T>(key: string, path: string, apply: (value: T) => void) {
      try { const value = await request<T>('/api/shared/' + path, { signal: active.signal }); if (!active.signal.aborted) apply(value) }
      catch (failure) { if (!active.signal.aborted) errors[key] = failure instanceof Error ? failure.message : 'Unavailable' }
    }
    const requests: Promise<void>[] = []
    if (hasNginx) requests.push(
      read<NginxStatus>('Nginx status', 'nginx/status', v => status = v),
      read<Site[]>('Sites', 'nginx/sites', v => sites = v),
      read<Settings>('Global settings', 'nginx/settings', v => settings = v),
      read<Log[]>('Logs', 'nginx/logs', v => { logs = v; if (!v.some(l => l.path === selectedLog)) selectedLog = v[0]?.path ?? '' }),
    )
    if (hasCertificates) requests.push(
      read<Certificate[]>('Certificates', 'certificates', v => certificates = v),
      read<HttpAccess>('HTTP validation', 'certificates/http-access', v => httpAccess = v),
      read<Lock>('Dehydrated', 'certificates/lock', v => lock = v),
    )
    await Promise.all(requests)
    if (!active.signal.aborted) loading = false
  }
  async function readLog() {
    if (!selectedLog || logLoading) return
    logController?.abort()
    const active = new AbortController(); logController = active
    logLoading = true; logError = ''; logText = ''
    try {
      const path = selectedLog.split('/').map(encodeURIComponent).join('/')
      const result = await request<{content: string}>('/api/shared/nginx/logs/' + path, { signal: active.signal })
      if (!active.signal.aborted) logText = result.content
    } catch (failure) { if (!active.signal.aborted) logError = failure instanceof Error ? failure.message : 'Could not read log' }
    finally { if (!active.signal.aborted) logLoading = false }
  }
  onMount(() => {
    if (!hasNginx && hasCertificates) section = 'certificates'
    void refresh()
    return () => { controller?.abort(); logController?.abort() }
  })
</script>

<div class="connection-tabs" aria-label="Web modules">
  <button class:active={section === 'nginx'} onclick={() => section = 'nginx'}>Nginx</button>
  <button class:active={section === 'certificates'} onclick={() => section = 'certificates'}>Certificates</button>
  <button disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
</div>
<p class="policy-note">Shared module inventory is read-only in the Linux session UI. Configuration changes and privileged service operations are not exposed here until Linux-specific authorization is available.</p>
{#each Object.entries(errors) as [name, error]}<p class="form-error" role="alert">{name}: {error}</p>{/each}

{#if section === 'nginx'}
  {#if !hasNginx}<section class="panel"><h2>Nginx unavailable</h2><p>Configure the Linux Nginx paths before inspecting its sites.</p></section>{:else}
    <section class="panel">
      <div class="resource-heading"><h2><Icon name="server" size={17} /> Nginx</h2><span class="project-state" class:running={status?.running}>{status ? (status.running ? 'Running' : 'Stopped') : 'Unknown'}</span></div>
      {#if status}<p class="muted mono">{status.config_path}</p>{/if}
      <p>Service lifecycle is managed from Services. Test and reload require an authorized Linux operation.</p>
    </section>
    <section class="panel">
      <div class="resource-heading"><h2>Manage sites</h2><span>{sites.length} sites</span></div>
      <label for="site-filter">Filter sites</label><input id="site-filter" type="search" bind:value={filter} placeholder="Find a domain or subfolder…" />
      <div class="shared-table"><table><thead><tr><th>Site</th><th>Port</th><th>Configuration</th><th>State</th></tr></thead><tbody>
      {#each visibleSites as site (site.kind + '/' + site.domain + '/' + site.name)}
        <tr><td><strong>{site.display_name || site.name}</strong><small>{site.kind}</small></td><td>{site.port ?? '—'}</td><td>{site.managed ? 'Managed' : 'Native'}</td><td>{site.enabled ? site.state : 'Disabled'}</td></tr>
      {/each}
      </tbody></table></div>
      {#if !visibleSites.length && !loading}<p class="muted">No matching sites.</p>{/if}
    </section>
    <section class="panel"><div class="resource-heading"><h2>Global settings</h2></div><p>Shared listeners, client allowlists and defaults inherited by managed sites.</p>
      {#if settings}<dl class="shared-stats"><div><dt>Listener profiles</dt><dd>{settings.listener_profiles}</dd></div><div><dt>Access policies</dt><dd>{settings.access_policies}</dd></div><div><dt>Limit policies</dt><dd>{settings.limit_policies}</dd></div><div><dt>Allowed IP entries</dt><dd>{settings.allowed_ips}</dd></div></dl>{/if}
    </section>
    <section class="panel"><div class="resource-heading"><h2>Nginx logs</h2></div>
      <p class="resource-hint">Log content is requested only when you select Read log.</p>
      <label for="nginx-log">Log file</label><select id="nginx-log" bind:value={selectedLog} disabled={logLoading}>{#each logs as log}<option value={log.path}>{log.path} ({log.size} bytes)</option>{/each}</select>
      <button disabled={!selectedLog || logLoading} onclick={readLog}>{logLoading ? 'Reading…' : 'Read log'}</button>
      {#if logError}<p class="form-error" role="alert">{logError}</p>{/if}
      {#if logText}<pre>{logText}</pre>{/if}
    </section>
  {/if}
{:else}
  {#if !hasCertificates}<section class="panel"><h2>Certificates unavailable</h2></section>{:else}
    <section class="panel"><h2>HTTP certificate access</h2>
      {#if httpAccess}<dl class="shared-stats"><div><dt>State</dt><dd>{httpAccess.state}</dd></div><div><dt>WAN control</dt><dd>{httpAccess.wan_control}</dd></div><div><dt>Names</dt><dd>{httpAccess.names.join(', ') || '—'}</dd></div><div><dt>Expires</dt><dd>{httpAccess.expires_at ? new Date(httpAccess.expires_at).toLocaleString() : '—'}</dd></div></dl>{:else}<p class="muted">No validation status available.</p>{/if}
    </section>
    <section class="panel"><h2>Dehydrated</h2><p>{lock ? (lock.locked ? 'Locked' : 'Unlocked') + ' · ' + lock.status : 'Status unavailable'}</p></section>
    <section class="panel"><div class="resource-heading"><h2>Let's Encrypt certificates</h2><span>{certificates.length} certificates</span></div>
      <div class="shared-table"><table><thead><tr><th>Certificate / domains</th><th>Method</th><th>Status</th><th>Expiry</th><th>Auto renew</th></tr></thead><tbody>
        {#each certificates as cert (cert.id)}<tr><td><strong>{cert.name}</strong><small>{cert.domains.join(', ')}</small></td><td>{cert.method}{cert.staging ? ' · staging' : ''}</td><td><span class="project-state" class:attention={cert.renewal_due || !cert.exists}>{!cert.exists ? 'Missing' : cert.renewal_due ? 'Renewal due' : 'Issued'}</span></td><td>{cert.expires_at ? new Date(cert.expires_at).toLocaleDateString() : '—'}</td><td>{cert.auto_renew ? 'Enabled' : 'Disabled'}</td></tr>{/each}
      </tbody></table></div>{#if !certificates.length && !loading}<p class="muted">No certificates found.</p>{/if}
    </section>
  {/if}
{/if}

<style>
  .shared-table { overflow-x: auto; margin-top: 1rem; }
  table { width: 100%; border-collapse: collapse; text-align: left; }
  th, td { padding: .7rem; border-bottom: 1px solid var(--border); }
  th { color: var(--muted); font-size: .78rem; }
  td small { display: block; color: var(--muted); overflow-wrap: anywhere; }
  .shared-stats { display: grid; grid-template-columns: repeat(auto-fit,minmax(10rem,1fr)); gap: 1rem; }
  .shared-stats dt { color: var(--muted); font-size: .85rem; }
  .shared-stats dd { margin: .25rem 0 0; overflow-wrap: anywhere; }
  pre { max-height: 24rem; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; background: var(--panel-inset); padding: 1rem; border-radius: var(--radius-sm); }
</style>
