<script lang="ts">
  import { onMount } from 'svelte'
  import Icon from './lib/Icon.svelte'
  import Login from './lib/Login.svelte'
  import ConfigurationHealth from './lib/ConfigurationHealth.svelte'
  import Jobs from './lib/Jobs.svelte'
  import Overview from './pages/Overview.svelte'
  import AgentAccess from './pages/AgentAccess.svelte'
  import Applications from './pages/Applications.svelte'
  import Services from './pages/Services.svelte'
  import Shares from './pages/ConfiguredShares.svelte'
  import Configuration from './pages/Configuration.svelte'
  import Updates from './pages/Updates.svelte'
  import { ApiError, request, type Status, type Discovery } from './lib/api'
  import type { SharedDiscovery } from './lib/shared'
  import { themeState } from './lib/theme.svelte'

  import Activity from './Activity.svelte'
  import Capabilities from './AgentAccess.svelte'
  const pages = [
    { id: 'overview', name: 'Overview', subtitle: 'Your server, at a glance.' },
    { id: 'applications', name: 'Applications', subtitle: 'Managed workloads and configuration health.' },
    { id: 'services', name: 'Services', subtitle: 'System services and their logs.' },
    { id: 'configuration', name: 'Configuration', subtitle: 'Storage, bindings, private references and recovery.' },
    { id: 'shares', name: 'File shares', subtitle: 'Samba and NFS exports.' },
    { id: 'updates', name: 'Updates', subtitle: 'Keep your Linux system up to date.' },
    { id: 'web', name: 'Web', subtitle: 'Nginx sites, certificates and logs.' },
    { id: 'security', name: 'Ban Shield', subtitle: 'Protection, active bans and detection rules.' },
    { id: 'activity', name: 'Activity', subtitle: 'Inspect management jobs and their output.' },
    { id: 'agent', name: 'Agent access', subtitle: 'Manage your server through MCP.' },
  ]
  const pageModules: Record<string, string> = { applications: 'workloads', configuration: 'workloads', services: 'systemd', shares: 'shares', updates: 'updates' }
  let discovery = $state<Discovery | null>(null)
  let shared = $state<SharedDiscovery | null>(null)
  const visiblePages = $derived(pages.filter(p => {
    if (p.id === 'web') return shared?.modules.some(m => ['nginx', 'certificates'].includes(m.id) && m.availability !== 'unsupported')
    if (p.id === 'security') return shared?.modules.some(m => m.id === 'ban_shield' && m.availability === 'ready')
    return !pageModules[p.id] || discovery?.modules.some(m => m.id === pageModules[p.id])
  }))
  let selected = $state('overview')
  let status = $state<Status | null>(null)
  const workloadWrites = $derived(!!status?.allowWrites && !!discovery?.modules.find(m => m.id === 'workloads')?.writable)
  function navigate(target: string) { selected = target; location.hash = target; menuOpen = false }
  let menuOpen = $state(false)
  let loading = $state(true)
  let error = $state('')
  let needsAuth = $state(false)
  const page = $derived(visiblePages.find(p => p.id === selected) ?? pages[0])

  async function connect() {
    loading = true
    error = ''
    try { const next = await request<Status>('/api/status'); discovery = await request<Discovery>('/api/tools'); shared = await request<SharedDiscovery>('/api/shared/modules'); status = next; needsAuth = false }
    catch (e) { if (e instanceof ApiError && e.status === 401) { needsAuth = true; status = null; discovery = null; shared = null } else { error = e instanceof Error ? e.message : 'Could not connect to the server' } }
    finally { loading = false }
  }

  async function signOut() {
    try { await request('/api/session', { method: 'DELETE' }); needsAuth = true; status = null; discovery = null; shared = null; menuOpen = false }
    catch (e) { error = e instanceof Error ? e.message : 'Could not sign out' }
  }

  onMount(() => {
    themeState.init()
    const selectHash = () => { const hash = location.hash.slice(1); if (hash === 'main') return; selected = hash === 'agents' ? 'agent' : hash || 'overview'; menuOpen = false }
    selectHash()
    window.addEventListener('hashchange', selectHash)
    const expired = () => { needsAuth = true; status = null; discovery = null; shared = null; menuOpen = false }
    window.addEventListener('maestro:unauthorized', expired)
    void connect()
    return () => { window.removeEventListener('maestro:unauthorized', expired); window.removeEventListener('hashchange', selectHash) }
  })
</script>

<svelte:head><title>{page.name} · Maestro</title></svelte:head>

<a class="skip-link" href="#main">Skip to content</a>
{#if needsAuth}
  <Login onSignedIn={async () => { needsAuth = false; await connect() }} />
{:else}
<div class="app-shell">
  <aside class:open={menuOpen} aria-label="Main navigation">
    <a class="brand" href="#overview" onclick={() => { navigate('overview') }}>
      <span class="brand-mark"><img src="/maestro.svg" alt="Maestro" width="38" height="38" /></span>
      <span>Maestro<span class="brand-caption">LINUX CONTROL</span></span>
    </a>
    <div class="nav-label">WORKSPACE</div>
    <nav>
      {#each visiblePages as p}
        <button class:active={page.id === p.id} aria-current={page.id === p.id ? 'page' : undefined} onclick={() => { navigate(p.id) }}>
          <Icon name={p.id} /><span>{p.name}</span>
        </button>
      {/each}
    </nav>
    <div class="sidebar-footer">
      <span class="connection-dot" class:connected={status && !error}></span>
      <div><strong>{status?.hostname ?? 'Local server'}</strong><span>{loading ? 'Connecting…' : error ? 'Connection failed' : 'Connected'}</span></div>
    </div>
  </aside>
  {#if menuOpen}<button class="nav-backdrop" aria-label="Close navigation" onclick={() => { menuOpen = false }}></button>{/if}
  <div class="workspace">
    <header class="topbar">
      <div class="breadcrumb"><button class="icon-button mobile-menu" aria-label="Toggle navigation" aria-expanded={menuOpen} onclick={() => { menuOpen = !menuOpen }}><Icon name="menu" /></button><span class="muted">Workspace</span><span class="breadcrumb-divider">/</span><span>{page.name}</span></div>
      <div class="topbar-actions"><button class="theme-toggle" aria-label={`Switch to ${themeState.current === 'dark' ? 'light' : 'dark'} mode`} title={`Current theme: ${themeState.current === 'dark' ? 'Dark' : 'Light'} (click to switch)`} onclick={() => themeState.toggle()}><Icon name={themeState.current === 'dark' ? 'sun' : 'moon'} size={18} /></button>{#if status}<Jobs allowWrites={status.allowWrites} /><button class="small-button sign-out" onclick={signOut}>Sign out</button>{/if}<div class="host-pill"><span class="connection-dot" class:connected={status && !error}></span><span>{status?.hostname ?? 'Connecting'}</span></div></div>
    </header>
    <main id="main">
      <div class="page-heading"><div><div class="eyebrow">SERVER WORKSPACE</div><h1>{page.name}</h1><p>{page.subtitle}</p></div><span class="badge" class:read-only={!status?.allowWrites}>{status?.allowWrites ? 'Management enabled' : 'Read only'}</span></div>
      {#if status && discovery && !loading && !error && discovery.modules.some(m => m.id === 'workloads')}<ConfigurationHealth navigate={navigate} />{/if}
      {#if error}
        <div class="notice error" role="alert"><div><strong>Could not connect</strong><p>{error}</p></div><button onclick={connect} disabled={loading}>Try again</button></div>
      {:else if loading}
        <section class="panel empty-state" aria-busy="true"><span class="loading-ring"></span><h2>Connecting to your server</h2><p>Reading server status…</p></section>
      {:else if page.id === 'overview'}
        {#if discovery?.modules.some(m => m.id === 'monitor' && m.available)}<Overview />{:else}<section class="panel"><p>Resource monitoring is unavailable.</p></section>{/if}
        {#if discovery}<section class="panel"><h2>Capabilities</h2><ul>{#each discovery.modules as module (module.id)}<li>{module.title}: {module.available ? 'Available' : (module.reason ?? 'Unavailable')}</li>{/each}</ul></section>{/if}
      {:else if page.id === 'agent' && status}
        <AgentAccess allowWrites={status.allowWrites} />
        {#if discovery}<Capabilities {status} {discovery} />{/if}
      {:else if page.id === 'applications' && status}
        <Applications allowWrites={workloadWrites} />
      {:else if page.id === 'services' && status}
        <Services allowWrites={true} />
      {:else if page.id === 'configuration' && status}
        <Configuration allowWrites={workloadWrites} />
      {:else if page.id === 'shares' && status}
        {#if discovery}<Shares allowWrites={workloadWrites} {discovery} {status} />{/if}
      {:else if page.id === 'web' && shared}
        {#await import('./pages/SharedWeb.svelte')}<p role="status">Loading Web…</p>{:then {default: Page}}<Page discovery={shared} />{:catch failure}<p role="alert">{String(failure)}</p>{/await}
      {:else if page.id === 'security'}
        {#await import('./pages/BanShield.svelte')}<p role="status">Loading Ban Shield…</p>{:then {default: Page}}<Page />{:catch failure}<p role="alert">{String(failure)}</p>{/await}
      {:else if page.id === 'activity' && status && discovery}
        <Activity {status} {discovery} />
      {:else if page.id === 'updates' && status}
        <Updates allowWrites={discovery?.permissions?.packageWrites.status === 'ready'} />
      {:else}
        <section class="panel empty-state"><span class="module-icon"><Icon name={selected} size={30} /></span><h2>{page.name}</h2><p>This module is next in the implementation plan. Controls will appear here when its backend is ready.</p><button onclick={() => { navigate('overview') }}>Back to overview</button></section>
      {/if}
      <footer class="page-footer"><span>Maestro<span class="muted"> / </span>{status?.version ?? 'dev'}</span><span>One place for your Linux server</span></footer>
    </main>
  </div>
</div>
{/if}
