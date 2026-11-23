<script lang="ts">
  import { onMount } from 'svelte'
  import { ApiError, request, type Discovery, type Status } from './lib/api'

  const pages = [
    { id: 'overview', title: 'Overview', load: () => import('./Overview.svelte') },
    {
      id: 'applications',
      title: 'Applications',
      module: 'workloads',
      load: () => import('./Applications.svelte'),
    },
    {
      id: 'services',
      title: 'Services',
      module: 'systemd',
      load: () => import('./Services.svelte'),
    },
    {
      id: 'shares',
      title: 'Shares',
      module: 'shares',
      load: () => import('./Shares.svelte'),
    },
    { id: 'updates', title: 'Updates', module: 'updates', load: () => import('./Updates.svelte') },
    { id: 'activity', title: 'Activity', load: () => import('./Activity.svelte') },
    { id: 'agents', title: 'Agent access', load: () => import('./AgentAccess.svelte') },
  ]
  let selected = $state('overview')
  const visiblePages = $derived(pages.filter(candidate =>
    !candidate.module || discovery?.modules.some(module => module.id === candidate.module)))
  const page = $derived(visiblePages.find(candidate => candidate.id === selected) ?? pages[0])
  let menuOpen = $state(false)
  let status = $state<Status | null>(null)
  let discovery = $state<Discovery | null>(null)
  let loading = $state(true)
  let needsAuth = $state(false)
  let error = $state('')
  let token = $state('')

  function signedOut() {
    status = null
    discovery = null
    needsAuth = true
    menuOpen = false
  }

  async function connect() {
    loading = true
    error = ''
    try {
      const result = await request<Status>('/api/status')
      discovery = await request<Discovery>('/api/tools')
      status = result
      needsAuth = false
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 401) signedOut()
      else error = message(failure)
    } finally {
      loading = false
    }
  }

  function message(failure: unknown) {
    return failure instanceof Error ? failure.message : 'Could not connect to Maestro.'
  }

  async function signIn(event: SubmitEvent) {
    event.preventDefault()
    loading = true
    error = ''
    try {
      await request('/api/session', { method: 'POST', body: JSON.stringify({ token }) })
      token = ''
      await connect()
    } catch (failure) {
      error = message(failure)
    } finally {
      loading = false
    }
  }

  async function signOut() {
    try {
      await request('/api/session', { method: 'DELETE' })
      signedOut()
      error = ''
    } catch (failure) {
      error = message(failure)
    }
  }

  onMount(() => {
    const select = () => {
      selected = location.hash.slice(1)
      menuOpen = false
    }
    select()
    window.addEventListener('hashchange', select)
    window.addEventListener('server-man:unauthorized', signedOut)
    void connect()
    return () => {
      window.removeEventListener('hashchange', select)
      window.removeEventListener('server-man:unauthorized', signedOut)
    }
  })
</script>

{#if needsAuth}
  <main class="login-screen">
    <form class="panel login-panel" onsubmit={signIn}>
      <img src="/maestro.svg" alt="" width="42" height="42">
      <h1>Sign in to Maestro</h1>
      <label for="token">Access token</label>
      <input id="token" type="password" autocomplete="current-password" bind:value={token} required>
      {#if error}<p role="alert">{error}</p>{/if}
      <button type="submit" disabled={loading}>{loading ? 'Signing in…' : 'Sign in'}</button>
    </form>
  </main>
{:else}
  <a class="skip-link" href="#main">Skip to content</a>
  <div class="app-shell">
    <aside class:open={menuOpen} aria-label="Main navigation" id="navigation">
      <a class="brand" href="#overview"><img src="/maestro.svg" alt="" width="38" height="38"> Maestro</a>
      <nav>
        {#each visiblePages as item (item.id)}
          <a href={'#' + item.id} aria-current={page.id === item.id ? 'page' : undefined} onclick={() => menuOpen = false}>{item.title}</a>
        {/each}
      </nav>
    </aside>
    <main id="main" tabindex="-1">
      <header>
        <button class="menu-toggle" type="button" onclick={() => menuOpen = !menuOpen} aria-expanded={menuOpen} aria-controls="navigation">Menu</button>
        <h1>{page.title}</h1>
        {#if status}<button type="button" onclick={signOut}>Sign out</button>{/if}
      </header>
      <div class="content">
        {#if error}
          <p role="alert">{error}</p>
          {#if !status}<button type="button" onclick={connect}>Retry</button>{/if}
        {/if}
        {#if loading}<p role="status">Connecting…</p>{/if}
        {#if status && discovery}
          {#await page.load()}
            <p role="status">Loading {page.title}…</p>
          {:then { default: Page }}
            <Page {status} {discovery} />
          {:catch failure}
            <p role="alert">{message(failure)}</p>
          {/await}
        {/if}
      </div>
    </main>
  </div>
{/if}
