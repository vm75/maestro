<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, type Discovery, type Status } from './lib/api'

  let { status: _status, discovery: _discovery }: { status: Status; discovery: Discovery } = $props()

  interface Share {
    id: string
    protocol: string
    path: string
    enabled: boolean
    readOnly: boolean
    users: string[]
    clients: string[]
  }

  interface ServiceStatus {
    configured: boolean
    available: boolean
    path?: string
  }

  interface SharesStatusResponse {
    samba: ServiceStatus
    nfs: ServiceStatus
  }

  let shares = $state<Share[]>([])
  let sharesStatus = $state<SharesStatusResponse | null>(null)
  let loading = $state(true)
  let error = $state('')

  async function loadData() {
    loading = true
    error = ''
    try {
      const [sharesRes, statusRes] = await Promise.all([
        invokeTool<{ shares: Share[] }>('shares_list', {}),
        invokeTool<SharesStatusResponse>('shares_status', {}),
      ])
      shares = sharesRes.shares
      sharesStatus = statusRes
    } catch (e) {
      error = e instanceof Error ? e.message : 'Could not load shares data'
    } finally {
      loading = false
    }
  }

  onMount(() => {
    void loadData()
  })
</script>

<div class="shares-container">
  <!-- Service Health & Status -->
  <section class="panel" aria-labelledby="status-heading">
    <h2 id="status-heading">Services Status</h2>
    {#if sharesStatus}
      <div class="services-status-grid">
        <article class="service-card">
          <div class="service-header">
            <h3>Samba (SMB)</h3>
            <span class="status-badge" class:configured={sharesStatus.samba.configured}>
              {sharesStatus.samba.configured ? 'Configured' : 'Not configured'}
            </span>
          </div>
          <p class="service-meta">
            Config: <code>{sharesStatus.samba.path || '/etc/samba/smb.conf'}</code>
          </p>
        </article>

        <article class="service-card">
          <div class="service-header">
            <h3>NFS (Network File System)</h3>
            <span class="status-badge" class:configured={sharesStatus.nfs.configured}>
              {sharesStatus.nfs.configured ? 'Configured' : 'Not configured'}
            </span>
          </div>
          <p class="service-meta">
            Exports: <code>{sharesStatus.nfs.path || '/etc/exports'}</code>
          </p>
        </article>
      </div>
    {:else if loading}
      <p role="status">Checking services status…</p>
    {/if}
  </section>

  <!-- Network Shares List -->
  <section class="panel" aria-labelledby="shares-heading">
    <div class="panel-header">
      <h2 id="shares-heading">Configured Shares</h2>
      <a href="#applications" class="edit-link">Edit in Workloads YAML</a>
    </div>

    {#if error}
      <p role="alert" class="error-msg">{error}</p>
    {/if}

    {#if loading}
      <p role="status">Loading shares…</p>
    {:else if shares.length === 0}
      <p class="empty-state">
        No network shares configured yet. Define shares under the <code>shares:</code> section of your declarative workloads YAML.
      </p>
    {:else}
      <div class="shares-grid">
        {#each shares as share (share.id)}
          <article class="share-card">
            <div class="share-header">
              <h3>{share.id}</h3>
              <div class="badges">
                <span class="protocol-badge protocol-{share.protocol}">{share.protocol.toUpperCase()}</span>
                <span class="perm-badge">{share.readOnly ? 'Read-Only' : 'Read/Write'}</span>
              </div>
            </div>
            <p class="share-path"><code>{share.path || share.id}</code></p>
            <div class="share-details">
              {#if share.protocol === 'samba'}
                <p>Users: <strong>{share.users.length ? share.users.join(', ') : 'Any / Guest'}</strong></p>
              {:else}
                <p>Clients: <strong>{share.clients.length ? share.clients.join(', ') : 'All'}</strong></p>
              {/if}
              <p>Enabled: <strong>{share.enabled ? 'Yes' : 'No'}</strong></p>
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .shares-container {
    display: grid;
    gap: 1.5rem;
  }
  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1rem;
  }
  .edit-link {
    font-size: 0.9rem;
    color: var(--accent);
    text-decoration: none;
  }
  .edit-link:hover {
    text-decoration: underline;
  }
  .services-status-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: 1rem;
  }
  .service-card {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
  }
  .service-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
  }
  .service-header h3 {
    margin: 0;
    font-size: 1.05rem;
  }
  .status-badge {
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
    background: #3b2828;
    color: #ff9b9b;
  }
  .status-badge.configured {
    background: #1c3b2b;
    color: #79f7a7;
  }
  .service-meta {
    margin: 0;
    font-size: 0.85rem;
    color: var(--muted);
  }
  .shares-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
    gap: 1rem;
  }
  .share-card {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .share-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .share-header h3 {
    margin: 0;
    font-size: 1.1rem;
  }
  .badges {
    display: flex;
    gap: 0.4rem;
  }
  .protocol-badge {
    padding: 0.15rem 0.45rem;
    border-radius: 3px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .protocol-samba {
    background: #1b354f;
    color: #7bbaff;
  }
  .protocol-nfs {
    background: #3b2848;
    color: #e2aaff;
  }
  .perm-badge {
    padding: 0.15rem 0.45rem;
    border-radius: 3px;
    font-size: 0.75rem;
    background: var(--panel);
    border: 1px solid var(--border);
  }
  .share-path {
    margin: 0;
    font-size: 0.9rem;
  }
  .share-details {
    font-size: 0.85rem;
    color: var(--muted);
    display: grid;
    gap: 0.25rem;
  }
  .share-details p {
    margin: 0;
  }
  .error-msg {
    color: #ff6b6b;
  }
  .empty-state {
    color: var(--muted);
    font-size: 0.95rem;
  }
</style>
