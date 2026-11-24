<script lang="ts">
  import { onMount } from 'svelte'
  import Icon from '../lib/Icon.svelte'
  import { request } from '../lib/api'
  import type { SharedDiscovery } from '../lib/shared'
  import NginxGlobalEditor from '../lib/NginxGlobalEditor.svelte'
  import NginxPolicyEditor from '../lib/NginxPolicyEditor.svelte'
  import {normalizeGlobal,clone,type GlobalSettings,type Settings,type EffectiveAccess,type Usage} from '../lib/nginx-config'

  let { discovery, allowWrites }: { discovery: SharedDiscovery; allowWrites: boolean } = $props()
  const hasNginx = $derived(discovery.modules.some(m => m.id === 'nginx' && m.availability === 'ready'))
  const hasCertificates = $derived(discovery.modules.some(m => m.id === 'certificates' && m.availability === 'ready'))
  let section = $state<'nginx' | 'certificates'>('nginx')
  interface NginxStatus { running: boolean; config_path: string; root_dir: string }
  interface Site { kind: string; domain: string; name: string; display_name: string; port?: number; managed: boolean; enabled: boolean; state: string; effective_access?: EffectiveAccess }
  interface Log { path: string; size: number }
  interface Certificate { id: string; name: string; domains: string[]; method: string; staging: boolean; auto_renew: boolean; exists: boolean; expires_at?: string; days_remaining?: number; renewal_due: boolean }
  interface HttpAccess { state: string; expires_at?: string; names: string[]; wan_control: string }
  interface Lock { locked: boolean; status: string }
  let status = $state<NginxStatus | null>(null)
  let sites = $state<Site[]>([])
  let settings = $state<GlobalSettings | null>(null)
  let globalDraft = $state<GlobalSettings | null>(null)
  let usage = $state<Usage>({})
  let preview = $state<EffectiveAccess | null>(null)
  let previewError = $state('')
  let previewBusy = $state(false)
  let previewController: AbortController | undefined
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
      read<GlobalSettings>('Global settings', 'nginx/settings', v => settings = normalizeGlobal(v)),
      read<Usage>('Ingress usage', 'nginx/ingress-usage', v => usage = v),
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
  let busy = $state(false)
  let actionError = $state('')
  let actionMessage = $state('')
  let siteKind = $state('domain')
  let siteDomain = $state('')
  let siteName = $state('')
  let siteUpstream = $state('')
  let editingSite = $state<Site | null>(null)
  let siteSettings = $state<Settings>({})
  let siteEnabled = $state(false)
  let siteServerNames = $state('')
  let siteContent = $state('')
  let customSite = $state(false)
  let certificateId = $state('')
  let certificateName = $state('')
  let certificateDomains = $state('')
  let certificateMethod = $state('http')
  let certificateStaging = $state(true)
  let certificateRenew = $state(false)
  const objectPath = (site: Site) => 'nginx/objects/' + [site.kind, site.domain, site.name].map(encodeURIComponent).join('/')
  async function mutate(path: string, method = 'POST', body?: unknown, timeout = 60_000) {
    if (!allowWrites || busy) return false
    busy = true; actionError = ''; actionMessage = ''
    try {
      const result = await request<{success?: boolean; message?: string; stderr?: string}>('/api/shared/' + path, {method, body: body === undefined ? undefined : JSON.stringify(body)}, timeout)
      if (result.success === false) throw new Error(result.stderr || 'Command failed')
      actionMessage = result.message || 'Operation completed'
      await refresh()
      return true
    } catch (failure) { actionError = failure instanceof Error ? failure.message : 'Operation failed'; return false }
    finally { busy = false }
  }
  async function editSite(site: Site) {
    if (busy) return
    actionError = ''; busy = true
    try {
      const detail = await request<{settings: Settings | null; content: string; upstream?: string; server_names: string[]}>('/api/shared/' + objectPath(site))
      editingSite = site; siteKind = site.kind; siteDomain = site.domain; siteName = site.name
      siteUpstream = detail.upstream || ''; customSite = !site.managed
      siteSettings = detail.settings || {}; preview = null; previewError = ''; siteContent = detail.content; siteServerNames = detail.server_names.join(' ')
    } catch (failure) { actionError = String(failure) }
    finally { busy = false }
  }
  function resetSite() { editingSite = null; siteDomain = ''; siteName = ''; siteUpstream = ''; siteSettings = {}; siteEnabled = false; preview = null; previewError = ''; previewController?.abort(); previewBusy = false; siteContent = ''; siteServerNames = ''; customSite = false }
  async function saveSite() {
    try {
      const body = { ...(customSite ? {content: siteContent} : {settings: siteSettings}), upstream: siteUpstream, server_names: siteServerNames.trim().split(/\s+/).filter(Boolean) }
      const saved = editingSite ? await mutate(objectPath(editingSite), 'PUT', body) :
        await mutate('nginx/objects/' + siteKind, 'POST', {...body, domain: siteDomain, name: siteKind === 'domain' ? siteDomain : siteName, enabled: siteEnabled})
      if (saved) resetSite()
    } catch (failure) { actionError = String(failure) }
  }
  async function saveGlobal(value: GlobalSettings) {
    if (await mutate('nginx/settings','PUT',value)) globalDraft = null
  }
  async function previewAccess() {
    previewController?.abort(); const active = new AbortController(); previewController = active
    previewBusy = true; preview = null; previewError = ''
    try {
      const result = await request<EffectiveAccess>('/api/shared/nginx/access-preview',{method:'POST',body:JSON.stringify({kind:siteKind,domain:siteDomain,settings:siteSettings}),signal:active.signal})
      if (!active.signal.aborted) preview = result
    } catch(e) { if (!active.signal.aborted) previewError = String(e) }
    finally { if (!active.signal.aborted) previewBusy = false }
  }
  function launchSite(site: Site) {
    const [host,...parts] = site.display_name.split('/')
    const port = site.effective_access?.https[0]?.port
    if (!host || !/^[A-Za-z0-9.-]+$/.test(host)) return
    window.open('https://'+host+(port && port!==443 ? ':'+port : '')+(parts.length ? '/'+parts.map(encodeURIComponent).join('/') : ''),'_blank','noopener,noreferrer')
  }
  function editCertificate(cert: Certificate) {
    certificateId = cert.id; certificateName = cert.name; certificateDomains = cert.domains.join(', ')
    certificateMethod = cert.method; certificateStaging = cert.staging; certificateRenew = cert.auto_renew
  }
  function resetCertificate() { certificateId = ''; certificateName = ''; certificateDomains = ''; certificateMethod = 'http'; certificateStaging = true; certificateRenew = false }
  async function saveCertificate() {
    await mutate('certificates' + (certificateId ? '/' + encodeURIComponent(certificateId) : ''), certificateId ? 'PUT' : 'POST', {
      name: certificateName, domains: certificateDomains.split(/[\s,]+/).filter(Boolean), method: certificateMethod, staging: certificateStaging, auto_renew: certificateRenew
    })
    if (!actionError) resetCertificate()
  }
  onMount(() => {
    if (!hasNginx && hasCertificates) section = 'certificates'
    void refresh()
    return () => { controller?.abort(); logController?.abort(); previewController?.abort() }
  })
</script>

<div class="connection-tabs" aria-label="Web modules">
  <button class:active={section === 'nginx'} onclick={() => section = 'nginx'}>Nginx</button>
  <button class:active={section === 'certificates'} onclick={() => section = 'certificates'}>Certificates</button>
  <button disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
</div>
<p class="policy-note">Manage Maestro's private Nginx instance and certificate definitions. DNS hooks and credentials remain operator-provisioned on the server.</p>
{#if actionError}<p class="form-error" role="alert">{actionError}</p>{/if}
{#if actionMessage}<p role="status">{actionMessage}</p>{/if}
{#each Object.entries(errors) as [name, error]}<p class="form-error" role="alert">{name}: {error}</p>{/each}

{#if section === 'nginx'}
  {#if !hasNginx}<section class="panel"><h2>Nginx unavailable</h2><p>Configure the Linux Nginx paths before inspecting its sites.</p></section>{:else}
    <section class="panel">
      <div class="resource-heading"><h2><Icon name="server" size={17} /> Nginx</h2><span class="project-state" class:running={status?.running}>{status ? (status.running ? 'Running' : 'Stopped') : 'Unknown'}</span></div>
      {#if status}<p class="muted mono">{status.config_path}</p>{/if}
      <div class="connection-tabs">
        {#each ['test', 'start', 'stop', 'reload'] as action}
          <button disabled={!allowWrites || busy || (action === 'start' && !!status?.running) || (['stop', 'reload'].includes(action) && !status?.running)} onclick={() => { if (action !== 'stop' || confirm('Stop Maestro Nginx?')) void mutate('nginx/' + action) }}>{action}</button>
        {/each}
      </div>
    </section>
    <section class="panel">
      <div class="resource-heading"><h2>Sites</h2><span>{sites.length} sites</span></div>
      <label for="site-filter">Filter sites</label><input id="site-filter" type="search" bind:value={filter} placeholder="Find a domain or subfolder…" />
      <div class="shared-table"><table><thead><tr><th>Site</th><th>Port</th><th>Configuration</th><th>State</th><th>Actions</th></tr></thead><tbody>
      {#each visibleSites as site (site.kind + '/' + site.domain + '/' + site.name)}
        <tr><td><strong>{site.display_name || site.name}</strong><small>{site.kind}</small></td><td>{site.port ?? '—'}</td><td>{site.managed ? 'Managed' : 'Native'}{#if site.effective_access}<small>{site.effective_access.listener_profile} · {site.effective_access.access_policy}</small><small>{site.effective_access.https.map(e=>e.address+':'+e.port).join(', ')}</small>{/if}</td><td>{site.enabled ? site.state : 'Disabled'}</td><td>
          <button disabled={!allowWrites || busy} onclick={() => editSite(site)}>Edit</button>
          <button type="button" onclick={() => launchSite(site)}>Launch</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (!site.enabled || confirm('Disable this site?')) void mutate(objectPath(site) + (site.enabled ? '/disable' : '/enable')) }}>{site.enabled ? 'Disable' : 'Enable'}</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Delete ' + site.display_name + '?')) void mutate(objectPath(site), 'DELETE') }}>Delete</button>
        </td></tr>
      {/each}
      </tbody></table></div>
      {#if !visibleSites.length && !loading}<p class="muted">No matching sites.</p>{/if}
      {#if allowWrites}
        <details open={!!editingSite}><summary>{editingSite ? 'Edit site' : 'Add site'}</summary>
          <form onsubmit={event => { event.preventDefault(); void saveSite() }}>
            <label>Kind <select bind:value={siteKind} disabled={!!editingSite} onchange={()=>{if(siteKind==='subfolder'){const next={...siteSettings};delete next.listener_profile;siteSettings=next}preview=null}}><option value="domain">Domain</option><option value="subdomain">Subdomain</option><option value="subfolder">Subfolder</option></select></label>
            <label>Domain <input bind:value={siteDomain} required disabled={!!editingSite} /></label>
            {#if siteKind !== 'domain'}<label>Name <input bind:value={siteName} required disabled={!!editingSite} /></label>{/if}
            <label>Upstream <input bind:value={siteUpstream} placeholder="http://127.0.0.1:8080" /></label>
            <label>Server names (optional, space separated) <input bind:value={siteServerNames} /></label>
            <label><input type="checkbox" bind:checked={customSite} disabled={!!editingSite} /> Custom Nginx configuration</label>
            {#if customSite}<label>Configuration <textarea bind:value={siteContent} rows="8" required></textarea></label>
            {:else if settings}
              {#key siteKind+'|'+(editingSite?.name ?? '')}
                <NginxPolicyEditor bind:value={siteSettings} global={settings} kind={siteKind} disabled={busy}/>
              {/key}
              <button type="button" disabled={previewBusy || !siteDomain} onclick={previewAccess}>{previewBusy ? 'Resolving…' : 'Preview effective access'}</button>
              {#if previewError}<p class="form-error">{previewError}</p>{/if}
              {#if preview}<div class="policy-note">
                <strong>Effective access (last preview)</strong>
                <p>Listener: {preview.listener_profile} · Access: {preview.access_policy}{preview.inherited_from ? ' · Parent: '+preview.inherited_from : ''}</p>
                <p>HTTPS: {preview.https.map(e=>e.address+':'+e.port).join(', ') || 'None'} · HTTP: {preview.http.map(e=>e.address+':'+e.port).join(', ') || 'None'}</p>
                <p>Allowed sources: {preview.allowed_sources === null ? 'Public' : preview.allowed_sources.join(', ') || 'Denied'}</p>
                {#each preview.routes as route}<p><code>{route.path}</code> · {route.access_policy} · {route.allowed_sources === null ? 'Public' : route.allowed_sources.join(', ') || 'Denied'}</p>{/each}
              </div>{/if}
            {:else}<p class="form-error">Load global settings before editing managed sites.</p>{/if}
            {#if !editingSite}<label><input type="checkbox" bind:checked={siteEnabled}/> Enable after creating</label>{/if}
            <button type="submit" disabled={busy || (!customSite && !settings)}>Save site</button><button type="button" onclick={resetSite} disabled={busy}>Reset</button>
          </form>
        </details>
      {/if}
    </section>
    <section class="panel"><div class="resource-heading"><h2>Global settings</h2></div><p>Shared listeners, client allowlists and defaults inherited by managed sites.</p>
      {#if settings}
        <dl class="shared-stats"><div><dt>Listener profiles</dt><dd>{Object.keys(settings.listener_profiles).length}</dd></div><div><dt>Access policies</dt><dd>{Object.keys(settings.access_policies).length}</dd></div><div><dt>Limit policies</dt><dd>{Object.keys(settings.limit_policies).length}</dd></div><div><dt>Allowed IP entries</dt><dd>{settings.allowed_ips.length}</dd></div></dl>
        {#if globalDraft}<NginxGlobalEditor initial={globalDraft} {usage} disabled={busy || !allowWrites} onSave={saveGlobal} onCancel={()=>globalDraft=null}/>
        {:else}<button disabled={!allowWrites || busy} onclick={()=>{if(settings)globalDraft=clone(settings)}}>Configure global settings</button>{/if}
      {/if}
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
    <section class="panel"><h2>HTTP certificate access</h2><button disabled={!allowWrites || busy || !httpAccess} onclick={() => { if (confirm('Close the managed HTTP validation window?')) void mutate('certificates/http-access', 'DELETE') }}>Close validation window</button>
      {#if httpAccess}<dl class="shared-stats"><div><dt>State</dt><dd>{httpAccess.state}</dd></div><div><dt>WAN control</dt><dd>{httpAccess.wan_control}</dd></div><div><dt>Names</dt><dd>{httpAccess.names.join(', ') || '—'}</dd></div><div><dt>Expires</dt><dd>{httpAccess.expires_at ? new Date(httpAccess.expires_at).toLocaleString() : '—'}</dd></div></dl>{:else}<p class="muted">No validation status available.</p>{/if}
    </section>
    <section class="panel"><h2>Dehydrated</h2><p>{lock ? (lock.locked ? 'Locked' : 'Unlocked') + ' · ' + lock.status : 'Status unavailable'}</p>
      <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Download/update Dehydrated?')) void mutate('certificates/dehydrated/update') }}>Update Dehydrated</button>
      <button disabled={!allowWrites || busy || !lock?.locked || lock.status === 'running'} onclick={() => { if (confirm('Clear the stale lock? Confirm no certificate operation is running.')) void mutate('certificates/lock/clear') }}>Clear stale lock</button>
    </section>
    <section class="panel"><div class="resource-heading"><h2>Let's Encrypt certificates</h2><span>{certificates.length} certificates</span></div>
      <div class="shared-table"><table><thead><tr><th>Certificate / domains</th><th>Method</th><th>Status</th><th>Expiry</th><th>Auto renew</th><th>Actions</th></tr></thead><tbody>
        {#each certificates as cert (cert.id)}<tr><td><strong>{cert.name}</strong><small>{cert.domains.join(', ')}</small></td><td>{cert.method}{cert.staging ? ' · staging' : ''}</td><td><span class="project-state" class:attention={cert.renewal_due || !cert.exists}>{!cert.exists ? 'Missing' : cert.renewal_due ? 'Renewal due' : 'Issued'}</span></td><td>{cert.expires_at ? new Date(cert.expires_at).toLocaleDateString() : '—'}</td><td>{cert.auto_renew ? 'Enabled' : 'Disabled'}</td><td>
          <button disabled={!allowWrites || busy} onclick={() => editCertificate(cert)}>Edit</button>
          <button disabled={!allowWrites || busy || !!lock?.locked || lock?.status === 'running'} onclick={() => { if (confirm((cert.exists ? 'Renew ' : 'Issue ') + cert.name + '?')) void mutate('certificates/' + cert.id + (cert.exists ? '/renew' : '/issue'), 'POST', undefined, 3_660_000) }}>{cert.exists ? 'Renew' : 'Issue'}</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Remove definition for ' + cert.name + '? Files are retained.')) void mutate('certificates/' + cert.id, 'DELETE') }}>Delete</button>
        </td></tr>{/each}
      </tbody></table></div>{#if !certificates.length && !loading}<p class="muted">No certificates found.</p>{/if}
      {#if allowWrites}
        <details open={!!certificateId}><summary>{certificateId ? 'Edit certificate' : 'Add certificate'}</summary>
          <form onsubmit={event => { event.preventDefault(); void saveCertificate() }}>
            <label>Name <input bind:value={certificateName} required /></label>
            <label>Domains (comma or space separated) <input bind:value={certificateDomains} required /></label>
            <label>Validation <select bind:value={certificateMethod}><option value="http">HTTP-01</option><option value="dns">DNS-01 (server hook required)</option></select></label>
            <label><input type="checkbox" bind:checked={certificateStaging} /> Staging</label>
            <label><input type="checkbox" bind:checked={certificateRenew} /> Auto renew</label>
            <button type="submit" disabled={busy}>Save certificate</button><button type="button" disabled={busy} onclick={resetCertificate}>Reset</button>
          </form>
        </details>
      {/if}
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
