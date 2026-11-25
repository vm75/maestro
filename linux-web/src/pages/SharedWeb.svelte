<script lang="ts">
  import { onMount, tick, untrack } from 'svelte'
  import Icon from '../lib/Icon.svelte'
  import { request } from '../lib/api'
  import type { SharedDiscovery } from '../lib/shared'
  import NginxGlobalEditor from '../lib/NginxGlobalEditor.svelte'
  import NginxPolicyEditor from '../lib/NginxPolicyEditor.svelte'
  import {normalizeGlobal,clone,type GlobalSettings,type Settings,proxyActive, type EffectiveAccess,type Usage} from '../lib/nginx-config'

  let { discovery, allowWrites, initialSection = 'nginx' }: { discovery: SharedDiscovery; allowWrites: boolean; initialSection?: 'nginx'|'certificates' } = $props()
  const hasNginx = $derived(discovery.modules.some(m => m.id === 'nginx' && m.availability === 'ready'))
  const hasCertificates = $derived(discovery.modules.some(m => m.id === 'certificates' && m.availability === 'ready'))
  let section = $state<'nginx' | 'certificates'>(untrack(()=>initialSection))
  interface NginxStatus { running: boolean; config_path: string; root_dir: string }
  interface Site { kind: string; domain: string; name: string; display_name: string; port?: number; managed: boolean; enabled: boolean; state: string; effective_access?: EffectiveAccess }
  interface Log { path: string; size: number }
  interface Certificate { id: string; name: string; domains: string[]; method: string; staging: boolean; auto_renew: boolean; exists: boolean; expires_at?: string; days_remaining?: number; renewal_due: boolean }
  interface HttpAccess { state: string; expires_at?: string; names: string[]; wan_control: string; error?: string }
  interface Lock { locked: boolean; status: string; path?: string; operation?: {certificate: string; action: string; status: string; started_at: string; finished_at?: string} }
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
  let logOutput = $state<HTMLPreElement>()
  const httpLabels: Record<string,string> = {closed:'HTTP closed',active:'Certificate validation in progress',always_on:'Always-on HTTP configured',cleanup_failed:'Cleanup failed / exposure unknown',expired_cleanup_pending:'Window expired; local cleanup pending',unknown:'HTTP state not yet verified'}
  const wanLabels: Record<string,string> = {external:'Externally managed',unsupported:'Automatic control unavailable',unconfigured:'Not configured',managed_ipv4_kernel_expiry_ipv6_blocked:'Router-managed IPv4 (IPv6 blocked)'}
  const httpWarning = $derived(!!httpAccess && ['cleanup_failed','expired_cleanup_pending','unknown'].includes(httpAccess.state))
  let certOperation = $state<{certificate:string;action:string} | null>(null)
  let certPollTimer: ReturnType<typeof setTimeout> | undefined
  let logError = $state('')
  let logLoading = $state(false)
  let controller: AbortController | undefined
  let logController: AbortController | undefined
  let lockController: AbortController | undefined
  let lockPolling = false
  async function pollLock() {
    if (section!=='certificates' || !hasCertificates || lockPolling || loading) return
    const active=new AbortController(); lockController=active; lockPolling=true
    try { const [value,http]=await Promise.all([request<Lock>('/api/shared/certificates/lock',{signal:active.signal}),request<HttpAccess>('/api/shared/certificates/http-access',{signal:active.signal})]); if(!active.signal.aborted && section==='certificates') {lock=value; httpAccess=http; delete errors['Dehydrated polling']} }
    catch(e){if(!active.signal.aborted)errors['Dehydrated polling']=String(e)}
    finally{lockPolling=false}
  }
  let sortKey = $state<'display_name'|'port'|'managed'|'state'>('display_name')
  let sortDirection = $state(1)
  function sortSites(key: typeof sortKey) { if (sortKey === key) sortDirection *= -1; else { sortKey = key; sortDirection = 1 } }
  const visibleSites = $derived.by(() => {
    const query = filter.toLowerCase()
    const matches = (site: Site) => (site.display_name + ' ' + site.kind + ' ' + site.domain).toLowerCase().includes(query)
    const compare = (a: Site,b: Site) => sortDirection * (sortKey === 'port' ? (a.port ?? 0)-(b.port ?? 0) : String(a[sortKey]).localeCompare(String(b[sortKey]))) || a.display_name.localeCompare(b.display_name)
    const parents = sites.filter(s=>s.kind==='domain').sort(compare)
    const children = sites.filter(s=>s.kind!=='domain').sort(compare)
    return [...parents.flatMap(p=>[...(matches(p) ? [p] : []), ...children.filter(c=>c.domain===p.domain && (p.enabled || !!query) && matches(c))]), ...children.filter(c=>!parents.some(p=>p.domain===c.domain)&&matches(c))]
  })
  let siteEditorOpen = $state(false)
  let referenceDialog = $state<HTMLDialogElement>()
  function openReference() { referenceDialog?.showModal() }
  function closeReference() { referenceDialog?.close() }
  let globalSection = $state<HTMLElement>()
  let siteEditor = $state<HTMLDetailsElement>()
  let globalFromSite = $state(false)
  function configureGlobal(fromSite = false) { globalFromSite=fromSite; if (settings && !globalDraft) globalDraft = clone(settings); globalSection?.scrollIntoView({block:'start'}) }
  function closeGlobal() { globalDraft=null; if(globalFromSite)siteEditor?.scrollIntoView({block:'start'}); globalFromSite=false }

  async function refresh() {
    lockController?.abort(); controller?.abort()
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
      if (!active.signal.aborted) {logText = result.content;await tick();if(logOutput && !active.signal.aborted)logOutput.scrollTop=logOutput.scrollHeight}
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
  let certificateStaging = $state(false)
  let certificateRenew = $state(true)
  const objectPath = (site: Site) => 'nginx/objects/' + [site.kind, site.domain, site.name].map(encodeURIComponent).join('/')
  async function mutate(path: string, method = 'POST', body?: unknown, timeout = 60_000) {
    if (!allowWrites || busy) return false
    busy = true; actionError = ''; actionMessage = ''
    try {
      const result = await request<{success?: boolean; message?: string; stderr?: string; stdout?: string; code?: number}>('/api/shared/' + path, {method, body: body === undefined ? undefined : JSON.stringify(body)}, timeout)
      if (result.success === false) throw new Error((result.stderr || result.stdout || '').trim() || (result.code != null ? 'Command failed (exit code '+result.code+')' : 'Command failed'))
      actionMessage = result.message || (result.success === true ? (result.stdout || result.stderr || '').trim() || 'Command completed successfully' : 'Operation completed')
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
      previewController?.abort(); previewBusy = false
      editingSite = site; siteEditorOpen = true; siteKind = site.kind; siteDomain = site.domain; siteName = site.name
      siteUpstream = detail.upstream || ''; customSite = !site.managed
      siteSettings = detail.settings || {}; preview = null; previewError = ''; siteContent = detail.content; siteServerNames = detail.server_names.join(' ')
    } catch (failure) { actionError = String(failure) }
    finally { busy = false }
  }
  function resetSite() { editingSite = null; siteDomain = ''; siteName = ''; siteUpstream = ''; siteSettings = {}; siteEnabled = false; preview = null; previewError = ''; previewController?.abort(); previewBusy = false; siteContent = ''; siteServerNames = ''; customSite = false }
  async function saveSite() {
    try {
      const body = { ...(customSite ? {content: siteContent} : {settings: siteSettings}), upstream: siteUpstream.trim(), server_names: siteKind === 'subfolder' ? [] : siteServerNames.trim().split(/\s+/).filter(Boolean) }
      const saved = editingSite ? await mutate(objectPath(editingSite), 'PUT', body) :
        await mutate('nginx/objects/' + siteKind, 'POST', {...body, domain: siteDomain.trim(), name: siteKind === 'domain' ? siteDomain.trim() : siteName.trim(), enabled: siteEnabled})
      if (saved) { resetSite(); siteEditorOpen=false }
    } catch (failure) { actionError = String(failure) }
  }
  async function saveGlobal(value: GlobalSettings) {
    if (await mutate('nginx/settings','PUT',value)) closeGlobal()
  }
  async function previewAccess(payload: string) {
    previewController?.abort(); const active = new AbortController(); previewController = active
    previewBusy = true; preview = null; previewError = ''
    try {
      const result = await request<EffectiveAccess>('/api/shared/nginx/access-preview',{method:'POST',body:payload,signal:active.signal})
      if (!active.signal.aborted) preview = result
    } catch(e) { if (!active.signal.aborted) previewError = String(e) }
    finally { if (!active.signal.aborted) previewBusy = false }
  }
  $effect(() => {
    const payload = JSON.stringify({kind:siteKind,domain:siteDomain,settings:siteSettings})
    const savedGlobal = settings
    const shouldPreview = section === 'nginx' && siteEditorOpen && !!siteDomain.trim() && !customSite && !!savedGlobal
    const canPreview = allowWrites
    const stored = editingSite?.effective_access
    previewController?.abort(); preview = null; previewError = ''; previewBusy = false
    if (!shouldPreview) return
    if (!canPreview) { preview=stored ?? null; return }
    const timer = setTimeout(() => void previewAccess(payload),180)
    return () => { clearTimeout(timer); previewController?.abort() }
  })
  function launchSite(site: Site) {
    const [host,...parts] = site.display_name.split('/')
    const port = site.effective_access?.https[0]?.port
    if (!host || !/^[A-Za-z0-9.-]+$/.test(host)) return
    window.open('https://'+host+(port && port!==443 ? ':'+port : '')+(parts.length ? '/'+parts.map(encodeURIComponent).join('/') : ''),'_blank','noopener,noreferrer')
  }
  async function certificateAction(cert: Certificate) {
    if (!allowWrites || busy || lock?.locked || lock?.status==='running') return
    const action=cert.exists?'renew':'issue'
    if (!confirm((cert.exists?'Renew ':'Issue ')+cert.name+'?')) return
    certOperation={certificate:cert.name,action}
    certPollTimer=setTimeout(()=>void pollLock(),500)
    try {await mutate('certificates/'+cert.id+'/'+action,'POST',undefined,3_660_000)}
    finally {clearTimeout(certPollTimer);certOperation=null;void pollLock()}
  }
  function editCertificate(cert: Certificate) {
    certificateId = cert.id; certificateName = cert.name; certificateDomains = cert.domains.join(', ')
    certificateMethod = cert.method; certificateStaging = cert.staging; certificateRenew = cert.auto_renew
  }
  function resetCertificate() { certificateId = ''; certificateName = ''; certificateDomains = ''; certificateMethod = 'http'; certificateStaging = false; certificateRenew = true }
  async function saveCertificate() {
    const saved = await mutate('certificates' + (certificateId ? '/' + encodeURIComponent(certificateId) : ''), certificateId ? 'PUT' : 'POST', {
      name: certificateName, domains: certificateDomains.split(/[\s,]+/).filter(Boolean), method: certificateMethod, staging: certificateStaging, auto_renew: certificateRenew
    })
    if (saved) resetCertificate()
  }
  onMount(() => {
    if (!hasNginx && hasCertificates) section = 'certificates'; else if (!hasCertificates && hasNginx) section = 'nginx'
    void refresh()
    const timer=setInterval(pollLock,15000)
    return () => { clearInterval(timer); clearTimeout(certPollTimer); controller?.abort(); logController?.abort(); previewController?.abort(); lockController?.abort() }
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
      <div class="shared-table"><table><thead><tr>{#each [{key:'display_name',label:'Site'},{key:'port',label:'Port'},{key:'managed',label:'Configuration'},{key:'state',label:'State'}] as column}<th aria-sort={sortKey===column.key ? (sortDirection===1?'ascending':'descending') : 'none'}><button type="button" onclick={()=>sortSites(column.key as typeof sortKey)}>{column.label} {sortKey===column.key?(sortDirection===1?'↑':'↓'):'↕'}</button></th>{/each}<th>Actions</th></tr></thead><tbody>
      {#each visibleSites as site (site.kind + '/' + site.domain + '/' + site.name)}
        <tr><td class:child-site={site.kind!=='domain'}><strong>{site.display_name || site.name}</strong><small>{site.kind}</small></td><td>{site.port ?? '—'}</td><td>{site.managed ? 'Managed' : 'Native'}{#if site.effective_access}<small>{site.effective_access.listener_profile} · {site.effective_access.access_policy}</small><small>{site.effective_access.https.map(e=>e.address+':'+e.port).join(', ')}</small>{#if site.effective_access.allowed_sources?.length===0}<small class="form-error">Blocked</small>{/if}{/if}</td><td>{site.enabled ? site.state : 'Disabled'}</td><td>
          <button disabled={busy} onclick={() => editSite(site)}>{allowWrites?'Edit':'Inspect'}</button>
          <button type="button" onclick={() => launchSite(site)}>Launch</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (!site.enabled || confirm('Disable this site?')) void mutate(objectPath(site) + (site.enabled ? '/disable' : '/enable')) }}>{site.enabled ? 'Disable' : 'Enable'}</button>
          <button disabled={!allowWrites || busy} onclick={() => { if (confirm((site.kind==='domain'?'This also deletes nested subdomain and subfolder definitions. ':'')+'Delete ' + site.display_name + '?')) void mutate(objectPath(site), 'DELETE') }}>Delete</button>
        </td></tr>
      {/each}
      </tbody></table></div>
      {#if !visibleSites.length && !loading}<p class="muted">No matching sites.</p>{/if}
      {#if allowWrites || editingSite}
        <details bind:this={siteEditor} bind:open={siteEditorOpen}><summary>{editingSite ? (allowWrites?'Edit site':'Inspect site') : 'Add site'}</summary>
          <button type="button" onclick={openReference}>Setup guide</button><button type="button" disabled={!settings} onclick={()=>configureGlobal(true)}>Global settings</button>
          <form onsubmit={event => { event.preventDefault(); void saveSite() }}><fieldset disabled={!allowWrites || busy}>
            <label>Kind <select bind:value={siteKind} disabled={!!editingSite} onchange={event=>{if(event.currentTarget.value==='subfolder'){const next={...siteSettings};delete next.listener_profile;siteSettings=next}preview=null;previewController?.abort();previewBusy=false}}><option value="domain">Domain</option><option value="subdomain">Subdomain</option><option value="subfolder">Subfolder</option></select></label>
            {#if siteKind==='domain' || editingSite}<label>Domain <input bind:value={siteDomain} required disabled={!!editingSite} /></label>{:else}<label>Parent domain <select bind:value={siteDomain} required><option value="">Choose a domain</option>{#each sites.filter(s=>s.kind==='domain') as parent}<option value={parent.domain}>{parent.domain}{parent.enabled?'':' (disabled)'}</option>{/each}</select></label>{/if}
            {#if siteKind !== 'domain'}<label>Name <input bind:value={siteName} required disabled={!!editingSite} /></label>{/if}
            {#if customSite || proxyActive(siteKind,siteSettings,settings?.site_defaults ?? {})}<label>Upstream <input bind:value={siteUpstream} placeholder="http://127.0.0.1:8080" /></label>{/if}
            {#if siteKind!=='subfolder'}<label>Server names (optional, space separated) <textarea rows="2" bind:value={siteServerNames}></textarea></label>{/if}
            <label><input type="checkbox" bind:checked={customSite} disabled={!!editingSite} /> Custom Nginx configuration</label>
            {#if customSite}<label>Configuration <textarea bind:value={siteContent} rows="8" required></textarea></label>
            {:else if settings}
              {#key siteKind+'|'+(editingSite?.name ?? '')}
                <NginxPolicyEditor bind:value={siteSettings} global={settings} kind={siteKind} disabled={busy || !allowWrites}/>
              {/key}
              {#if previewBusy}<p role="status">Resolving effective access…</p>{/if}
              {#if previewError}<p class="form-error">{previewError}</p>{/if}
              {#if preview}<div class="policy-note">
                <strong>Effective access</strong>
                <p>Listener: {preview.listener_profile} · Access: {preview.access_policy}{preview.inherited_from ? ' · Parent: '+preview.inherited_from : ''}</p>
                <p>{preview.inherited_from ? 'The parent domain restricts allowed sources and controls the listener; proxy settings and routes inherit global defaults.' : ''}</p>
                <p>HTTPS: {preview.https.map(e=>e.address+':'+e.port).join(', ') || 'None'} · HTTP: {preview.http.map(e=>e.address+':'+e.port).join(', ') || 'None'}</p>
                <p>Allowed sources: {preview.allowed_sources === null ? 'Public' : preview.allowed_sources.join(', ') || 'Denied'}</p>{#if siteKind!=='domain' && sites.some(s=>s.kind==='domain' && s.domain===siteDomain && !s.enabled)}<p class="form-error">Containing domain is disabled.</p>{/if}
                {#each (proxyActive(siteKind,siteSettings,settings?.site_defaults ?? {})?preview.routes:[]) as route}<p><code>{route.path}</code> · {route.access_policy} · {route.allowed_sources === null ? 'Public' : route.allowed_sources.join(', ') || 'Denied'}</p>{/each}
              </div>{/if}
            {:else}<p class="form-error">Load global settings before editing managed sites.</p>{/if}
            {#if !editingSite}<label><input type="checkbox" bind:checked={siteEnabled}/> Enable after creating</label>{/if}
            {#if allowWrites}<button type="submit" disabled={busy || (!customSite && !settings)}>Save site</button>{/if}</fieldset>
            <button type="button" onclick={()=>{resetSite();siteEditorOpen=false}} disabled={busy}>Close editor</button>
          </form>
        </details>
      {/if}
    </section>
    <section class="panel"><div class="resource-heading"><h2>Setup quick reference</h2><button type="button" onclick={openReference}>Open guide</button></div><p>Settings for PWAs, WordPress, and media servers.</p></section>
    <section class="panel" bind:this={globalSection}><div class="resource-heading"><h2>Global settings</h2></div><p>Shared listeners, client allowlists and defaults inherited by managed sites.</p>
      {#if settings}
        <dl class="shared-stats"><div><dt>Listener profiles</dt><dd>{Object.keys(settings.listener_profiles).length}</dd></div><div><dt>Access policies</dt><dd>{Object.keys(settings.access_policies).length}</dd></div><div><dt>Limit policies</dt><dd>{Object.keys(settings.limit_policies).length}</dd></div><div><dt>Allowed IP entries</dt><dd>{settings.allowed_ips.length}</dd></div></dl>
        {#if globalDraft}<NginxGlobalEditor initial={globalDraft} {usage} disabled={busy || !allowWrites} onSave={saveGlobal} onCancel={closeGlobal}/>
        {:else}<button disabled={busy} onclick={()=>configureGlobal()}>{allowWrites?'Configure global settings':'Inspect global settings'}</button>{/if}
      {/if}
    </section>
    <section class="panel"><div class="resource-heading"><h2>Nginx logs</h2></div>
      <p class="resource-hint">Log content is requested only when you select Read log.</p>
      <label for="nginx-log">Log file</label><select id="nginx-log" bind:value={selectedLog} disabled={logLoading}>{#each logs as log}<option value={log.path}>{log.path} ({log.size} bytes)</option>{/each}</select>
      <button disabled={!selectedLog || logLoading} onclick={readLog}>{logLoading ? 'Reading…' : 'Read log'}</button>
      {#if logError}<p class="form-error" role="alert">{logError}</p>{/if}
      {#if logText}<pre bind:this={logOutput}>{logText}</pre>{/if}
    </section>
  {/if}
{:else}
  {#if !hasCertificates}<section class="panel"><h2>Certificates unavailable</h2></section>{:else}
    <section class="panel"><h2>HTTP certificate access</h2><button disabled={!allowWrites || busy || !httpAccess} onclick={() => { if (confirm('Close the managed HTTP validation window?')) void mutate('certificates/http-access', 'DELETE') }}>Close validation window</button>
      {#if httpAccess}<dl class="shared-stats"><div><dt>State</dt><dd class:form-error={httpWarning}>{httpLabels[httpAccess.state] || httpAccess.state}</dd></div><div><dt>WAN control</dt><dd>{wanLabels[httpAccess.wan_control] || httpAccess.wan_control}</dd></div><div><dt>Names</dt><dd>{httpAccess.names.join(', ') || '—'}</dd></div><div><dt>Expires</dt><dd>{httpAccess.expires_at ? new Date(httpAccess.expires_at).toLocaleString() : '—'}</dd></div></dl>{#if httpWarning || httpAccess.error}<p class="form-error" role="alert">{httpLabels[httpAccess.state] || 'HTTP validation warning'}{httpAccess.error ? ' · '+httpAccess.error : ''}. Verify local listeners and externally managed ingress; close the validation window if needed.</p>{/if}{:else}<p class="muted">No validation status available.</p>{/if}
    </section>
    <section class="panel"><h2>Dehydrated</h2><p>{lock ? (lock.locked ? 'Locked' : 'Unlocked') + ' · ' + lock.status : 'Status unavailable'}</p>
      {#if certOperation}<p role="status">Certificate {certOperation.action==='issue'?'issuance':'renewal'} in progress for {certOperation.certificate}. Issue and renewal are temporarily unavailable.</p>{:else if lock?.status==='locked'}<p class="form-error" role="alert">A lock remains without an active process. Clear it only after confirming no certificate hook is running.</p>{/if}
      {#if lock?.operation}<p>Certificate {lock.operation.action}: {lock.operation.certificate} · {lock.operation.status}{#if lock.operation.status==='failed'} — inspect server logs for details.{/if}</p>{/if}
      {#if lock?.locked && lock.path}<p class="mono">{lock.path}</p>{/if}
      <button disabled={!allowWrites || busy || !!lock?.locked || lock?.status==='running'} onclick={() => { if (confirm('Download/update Dehydrated?')) void mutate('certificates/dehydrated/update') }}>Update Dehydrated</button>
      <button disabled={!allowWrites || busy || !lock?.locked || lock.status === 'running'} onclick={() => { if (confirm('Clear the stale lock? Confirm no certificate operation is running.')) void mutate('certificates/lock/clear') }}>Clear stale lock</button>
    </section>
    <section class="panel"><div class="resource-heading"><h2>Let's Encrypt certificates</h2><span>{certificates.length} certificates</span></div>
      <div class="shared-table"><table><thead><tr><th>Certificate / domains</th><th>Method</th><th>Status</th><th>Expiry</th><th>Auto renew</th><th>Actions</th></tr></thead><tbody>
        {#each certificates as cert (cert.id)}<tr><td><strong>{cert.name}</strong><small>{cert.domains.join(', ')}</small></td><td>{cert.method}{cert.staging ? ' · staging' : ''}</td><td><span class="project-state" class:attention={cert.renewal_due || !cert.exists}>{!cert.exists ? 'Missing' : cert.renewal_due ? 'Renewal due' : 'Issued'}</span></td><td>{cert.expires_at ? new Date(cert.expires_at).toLocaleDateString() : '—'}{#if cert.days_remaining!=null}<small>{cert.days_remaining} days remaining</small>{/if}</td><td>{cert.auto_renew ? 'Enabled' : 'Disabled'}</td><td>
          <button disabled={!allowWrites || busy} onclick={() => editCertificate(cert)}>Edit</button>
          <button disabled={!allowWrites || busy || !!lock?.locked || lock?.status === 'running'} onclick={() => certificateAction(cert)}>{cert.exists ? 'Renew' : 'Issue'}</button>
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

<dialog bind:this={referenceDialog} class="confirm-dialog reference-dialog" aria-label="Nginx setup quick reference"><div class="resource-heading"><h2>Nginx setup quick reference</h2><button type="button" onclick={closeReference}>Close guide</button></div><p class="muted">Starting points for managed sites. Use the field names below in the site editor; set explicit values when your global defaults differ.</p>
        <h4>Connect the app</h4>
        <ol>
          <li>Choose a domain, subdomain, or subfolder and set <strong>Upstream URL</strong> to the app's internal HTTP/HTTPS address.</li>
          <li>Choose <strong>Access policy</strong>. A public site needs Public; a private site needs an allowlist in Global settings. A child cannot remove its parent domain's LAN restriction.</li>
          <li>For a domain that serves an app, set <strong>Return 404 at domain root</strong> to <strong>No</strong>. Enable subfolders only when needed. Enable a child's parent domain before enabling the child, and configure its TLS certificate.</li>
        </ol>
        <h4>Choose a profile</h4>
        <!-- svelte-ignore a11y_no_noninteractive_tabindex (Keyboard access to horizontally scrollable table.) -->
        <div class="shared-table" tabindex="0" role="region" aria-label="Site profiles; scroll horizontally on small screens">
          <table class="table">
            <thead><tr><th scope="col">Use case</th><th scope="col">Cache policy</th><th scope="col">Stream responses</th><th scope="col">Byte ranges</th><th scope="col">Long-lived connections</th></tr></thead>
            <tbody>
              <tr><th scope="row">PWA without media</th><td><code>pwa</code></td><td>No</td><td>No</td><td>No</td></tr>
              <tr><th scope="row">PWA with media</th><td><code>pwa</code></td><td>Yes</td><td>Yes</td><td>Yes</td></tr>
              <tr><th scope="row">WordPress without media</th><td><code>inherit</code></td><td>No</td><td>No</td><td>No</td></tr>
              <tr><th scope="row">WordPress with photos</th><td><code>inherit</code></td><td>No</td><td>No</td><td>No</td></tr>
              <tr><th scope="row">WordPress with video/audio</th><td><code>inherit</code></td><td>Yes</td><td>Yes</td><td>Yes</td></tr>
              <tr><th scope="row">Media server: video, photo, audio, books</th><td><code>inherit</code></td><td>Yes</td><td>Yes</td><td>Yes</td></tr>
            </tbody>
          </table>
        </div>
        <p class="muted">Set <strong>Cache policy</strong> explicitly as shown. PWA profiles need versioned static asset URLs because they receive long-lived cache headers. <code>inherit</code> adds no cache policy; the app's headers still apply. Use <code>no_cache</code> on private/authentication routes. For small photo/book libraries, streaming and long-lived connections are optional; byte ranges help seeking and PDF/download resumes.</p>
        <h4>Uploads, playback, and authentication</h4>
        <ul>
          <li><strong>Body size limit</strong> caps request uploads, not downloads. <code>128M</code> is a starting upload cap; match it to the app's needs.</li>
          <li>For big media/library uploads, set <strong>Large uploads = Yes</strong> and choose <strong>Large upload limit</strong>, for example <code>1G</code>. This separate limit replaces Body size limit while enabled. Match backend/PHP upload limits too.</li>
          <li><strong>Stream responses</strong> delivers data without response buffering. <strong>Byte ranges</strong> supports seeking/resuming. Long-lived connections and large uploads use <strong>Session timeout</strong> (default <code>3600s</code>); large uploads also use <strong>Upload idle timeout</strong> (default <code>3600s</code>).</li>
          <li>WebSockets work with the default <strong>Transport = websocket</strong>. Use <code>sse</code> only on an event-stream route. For backends needing HTTP authentication passthrough, such as Calibre, set <strong>Backend authentication = Yes</strong>.</li>
        </ul>
        <details class="nginx-reference-example">
          <summary>PWA / SPA: keep API errors real and admin pages private</summary>
          <p>Only if the app uses client-side routes, set <strong>SPA fallback URI</strong> to <code>/index.html</code>. Exclude its API/auth paths from that fallback. In <strong>Route policies (JSON, optional)</strong>, adapt these paths and merge with any existing routes:</p>
          <pre>{"[\n  {\"path\":\"/api/\",\"spa_fallback\":\"\",\"cache_policy\":\"no_cache\",\"enable_byte_ranges\":false},\n  {\"path\":\"/admin\",\"enable_lanonly\":true,\"spa_fallback\":\"\",\"cache_policy\":\"no_cache\",\"enable_byte_ranges\":false}\n]"}</pre>
          <p class="muted">Do not use SPA fallback for WordPress. A mounted app must support its public subfolder/base URL. Route paths are relative to the app: use <code>/admin</code>, not <code>/myapp/admin</code>.</p>
        </details>
        <details class="nginx-reference-example">
          <summary>WordPress: uncached admin/login and login rate limiting</summary>
          <p>Set <strong>Cache policy = inherit</strong>. Add these entries to <strong>Route policies (JSON, optional)</strong>; the bundled login zone is ready to use:</p>
          <pre>{"[\n  {\"path\":\"/wp-admin/\",\"cache_policy\":\"no_cache\",\"enable_byte_ranges\":false},\n  {\"path\":\"/wp-login.php\",\"match\":\"exact\",\"cache_policy\":\"no_cache\",\"enable_byte_ranges\":false,\"rate_limit\":{\"zone\":\"wplogin\",\"burst\":3}}\n]"}</pre>
          <p class="muted">Normal pages follow the backend's cache headers. Login/admin routes get no-store headers. Configure actual user authentication in WordPress.</p>
        </details>
        <details class="nginx-reference-example">
          <summary>Live updates: add an SSE route</summary>
          <p>If the app exposes an SSE endpoint, adapt its path and merge this entry into <strong>Route policies (JSON, optional)</strong>:</p>
          <pre>{"[\n  {\"path\":\"/api/updates\",\"proxy_transport\":\"sse\",\"cache_policy\":\"no_cache\",\"enable_byte_ranges\":false,\"spa_fallback\":\"\",\"proxy_session_timeout\":\"300s\"}\n]"}</pre>
          <p class="muted">This clears Connection and disables response buffering on that route. Other routes retain WebSocket support. Keep <strong>Clear Origin header = No</strong> unless the backend specifically requires otherwise.</p>
        </details>
      </dialog>
<style>
  .reference-dialog{max-width:min(60rem,95vw);max-height:90vh;overflow:auto}
  .child-site{padding-left:1.5rem}
  fieldset{border:0;padding:0;margin:0;min-width:0}
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
