<script lang="ts">
  import { onMount } from 'svelte'
  import DefinitionEditor from '../lib/DefinitionEditor.svelte'
  import ConfirmDialog from '../lib/ConfirmDialog.svelte'
  import NativeShares from '../Shares.svelte'
  import { invokeTool, startJob, type Job, type Discovery, type Status } from '../lib/api'
  import type { ShareDefinition } from '../lib/managed'
  let { allowWrites, discovery, status }: { allowWrites: boolean; discovery: Discovery; status: Status } = $props()
  interface ServerStatus { available: boolean; discoveryAvailable?: boolean; setupRequired?: boolean }
  interface ShareItem { id: string; definition: ShareDefinition; configuration: string; issues: { message: string; remedy: string; code?: string }[] }
  const canPublish = $derived(allowWrites && discovery.permissions?.shareWrites?.status === 'ready')
  const canSetup = $derived(canPublish && discovery.permissions?.packageWrites.status === 'ready')
  let shares = $state<ShareItem[]>([])
  let servers = $state<Record<string, ServerStatus>>({})
  let editor = $state<{ id: string } | null>(null)
  let error = $state('')
  let feedback = $state('')
  let loading = $state(false)
  let activeJob = $state<string | null>(null)
  let pending = $state<ShareItem | null>(null)
  let setupPending = $state(false)
  let changeNotify = $state(true)
  let sambaUsername = $state('')
  let protocol = $state('samba')
  let controller: AbortController | undefined
  let stopped = false

  const neededProtocols = ['samba', 'nfs']
  async function load() {
    controller?.abort(); controller = new AbortController()
    const signal = controller.signal
    loading = true; error = ''
    try {
      const result = await invokeTool<{ shares: ShareItem[]; servers?: Record<string, ServerStatus>; sambaUsername?: string }>('managed_shares', {}, signal)
      if (!stopped && !signal.aborted) {
        shares = result.shares
        sambaUsername = result.sambaUsername ?? ''
        if (result.servers) servers = result.servers
        const native = await invokeTool<{backends: {protocol: string; available: boolean; setupRequired: boolean}[]}>('shares_list', {}, signal)
        if (!stopped && !signal.aborted) servers = Object.fromEntries(native.backends.map(b => [b.protocol, b]))
      }
    }
    catch (e) { if (!signal.aborted) error = e instanceof Error ? e.message : 'Could not read file shares' }
    finally { if (!signal.aborted) loading = false }
  }
  async function publish() {
    const share = pending
    if (!share || !canPublish) return
    pending = null
    try { const job = await startJob('managed_share_publish', { share: share.id, published: !share.definition.enabled, confirm: true }); activeJob = job.id; feedback = 'Share publication change started.' }
    catch (e) { error = e instanceof Error ? e.message : 'Could not change publication' }
  }
  async function setup() {
    setupPending = false
    if (!canSetup) return
    try { const job = await startJob('shares_setup', { protocol, confirm: true, changeNotify }); activeJob = job.id; feedback = 'Host share-server setup started.' }
    catch (e) { error = e instanceof Error ? e.message : 'Could not start host setup' }
  }
  onMount(() => {
    const finished = (e: Event) => {
      const job = (e as CustomEvent<Job>).detail
      if (job.id !== activeJob) return
      activeJob = null
      feedback = job.state === 'succeeded' ? 'Share configuration applied. Existing clients may retain earlier access until reconnecting.' : job.error || `Operation ${job.state}.`
      void load()
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && editor !== null) {
        editor = null
      }
    }
    window.addEventListener('keydown', onKey)
    window.addEventListener('maestro:job-finished', finished)
    void load()
    return () => { stopped = true; controller?.abort(); window.removeEventListener('keydown', onKey); window.removeEventListener('maestro:job-finished', finished) }
  })
</script>

<div class="shares-toolbar">
  <button class="primary" disabled={!allowWrites || activeJob !== null} onclick={() => { editor = { id: '' } }}>Define file share</button>
  <button disabled={loading} onclick={load}>{loading ? 'Reading…' : 'Refresh shares'}</button>
</div>

{#if error}<div class="notice error" role="alert"><p>{error}</p><button onclick={load}>Retry</button></div>{/if}
{#if feedback}<div class="job-feedback" role="status">{feedback}</div>{/if}

{#if Object.keys(servers).length > 0}
  <section class="panel host-server-card">
    <h2>Host share server</h2>
    <p>
      {#if protocol === 'samba' && servers.samba?.available && !servers.samba?.setupRequired && servers.samba?.discoveryAvailable === false}
        Samba is available, but the WS-Discovery server service could not be found. Install or repair the <code>wsdd</code> server so Windows can discover shares in Network.
      {:else}
        Set up the native server before saving an enabled share. This prepares the Samba managed include or NFS export directory. Installing the privileged helper only authorizes this operation.
      {/if}
    </p>
    <div class="host-server-controls">
      <label class="protocol-select">
        <span>Server protocol</span>
        <select bind:value={protocol}>
          {#each neededProtocols as p}
            <option value={p}>{p === 'samba' ? 'Samba' : 'NFS'}</option>
          {/each}
        </select>
      </label>
      {#if protocol === 'samba'}
        <label><input type="checkbox" bind:checked={changeNotify} /> Notify clients of filesystem changes (all Samba shares)</label>
      {/if}
      <button disabled={!canSetup || activeJob !== null} onclick={() => { setupPending = true }}>
        {protocol === 'samba' && servers.samba?.available && !servers.samba?.setupRequired && servers.samba?.discoveryAvailable === false ? 'Install wsdd…' : 'Set up server…'}
      </button>
    </div>
  </section>
{/if}

{#if !canPublish}<p class="policy-note">{discovery.permissions?.shareWrites?.reason ?? "Native share publication requires authorized helper grants."}</p>{/if}
<p class="resource-hint intro-hint">Authenticated Samba shares use the Linux account running Maestro{ sambaUsername ? ` (${sambaUsername})` : '' }. Use the Samba account password form below to set its password. Host packages, daemon setup and directory permissions are managed separately.</p>

{#if editor}
  <div class="modal-backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) editor = null }}>
    <div class="modal-card">
      {#key editor.id}
        <DefinitionEditor kind="share" resourceID={editor.id} onClose={() => { editor = null }} onSaved={(id) => { editor = null; activeJob = id; feedback = 'Share definition save started. Follow Activity for validation and publication.' }} />
      {/key}
    </div>
  </div>
{/if}

{#if !shares.length && !loading}
  <section class="panel empty-state">
    <h2>No configured file shares</h2>
    <p>Add directory storage in Configuration, then define a Samba or NFS share.</p>
    <button class="primary" disabled={!allowWrites} onclick={() => { editor = { id: '' } }}>Define file share</button>
  </section>
{/if}

{#if shares.length}
  <div class="share-grid">
    {#each shares as share (share.id)}
      <section class="panel share-card">
        <header class="share-header">
          <div>
            <h2>{share.id}</h2>
            <span class="share-meta-line">{share.definition.protocol.toUpperCase()} · storage: {share.definition.storage}</span>
          </div>
          <span class="project-state" class:running={share.definition.enabled}>{share.configuration}</span>
        </header>

        <div class="share-status-chips">
          <span class="status-chip" class:enabled={share.definition.enabled}>
            {share.definition.enabled ? 'Publication enabled' : 'Publication disabled'}
          </span>
          <span class="status-chip">
            {share.definition.readOnly ? 'Read only' : 'Read and write'}
          </span>
        </div>

        {#if share.issues.length}
          <ul class="issues">
            {#each share.issues as issue}
              <li class="issue-item">
                <strong>{issue.message}</strong>
                <p>{issue.remedy}</p>
              </li>
            {/each}
          </ul>
        {/if}

        <div class="share-card-actions">
          <button disabled={!allowWrites || activeJob !== null} onclick={() => { editor = { id: share.id } }}>Edit share</button>
          <button disabled={!canPublish || activeJob !== null} onclick={() => { pending = share }}>
            {share.definition.enabled ? 'Unpublish share' : 'Publish share'}
          </button>
        </div>
      </section>
    {/each}
  </div>
{/if}

<ConfirmDialog open={pending !== null} title={pending ? `${pending.definition.enabled ? 'Unpublish' : 'Publish'}: ${pending.id}` : ''} description="Update the authoritative share publication setting. Shared data and host daemon enablement are preserved. Existing clients may keep prior settings until reconnecting." label="Apply publication" busy={activeJob !== null} onConfirm={publish} onClose={() => { pending = null }} />
<ConfirmDialog open={setupPending} title="Set up server" description="Install native server packages, prepare owned include/export setup, and enable/start the host server. Existing configuration and shared data are preserved. Credentials, firewall and directory permissions remain explicit administrator setup." label="Set up server" busy={activeJob !== null} onConfirm={setup} onClose={() => { setupPending = false }} />

<style>
  .shares-toolbar {
    display: flex;
    gap: 12px;
    align-items: center;
    margin-bottom: 24px;
    flex-wrap: wrap;
  }
  .host-server-card {
    margin-bottom: 16px;
  }
  .host-server-card h2 {
    font-size: 17px;
    margin-bottom: 6px;
  }
  .host-server-controls {
    display: flex;
    align-items: flex-end;
    gap: 14px;
    margin-top: 16px;
    flex-wrap: wrap;
  }
  .protocol-select {
    display: grid;
    gap: 6px;
    min-width: 160px;
    margin-bottom: 0;
  }
  .intro-hint {
    margin: 8px 0 24px;
  }
  .share-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 14px;
  }
  .share-header h2 {
    font-size: 18px;
    overflow-wrap: anywhere;
  }
  .share-meta-line {
    font-size: 12px;
    color: var(--muted);
    display: block;
    margin-top: 3px;
  }
  .share-status-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin: 14px 0 16px;
  }
  .status-chip {
    font-size: 11px;
    font-weight: 550;
    padding: 3px 8px;
    border-radius: 4px;
    background: var(--panel-subtle);
    border: 1px solid var(--border-subtle);
    color: var(--muted);
  }
  .status-chip.enabled {
    background: var(--accent-dim);
    border-color: var(--accent-border);
    color: var(--accent);
  }
  .issues {
    list-style: none;
    padding: 0;
    margin: 14px 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .issue-item {
    background: var(--amber-dim);
    border: 1px solid var(--amber-border);
    border-radius: var(--radius-sm);
    padding: 10px 14px;
  }
  .issue-item strong {
    color: var(--amber-text);
    font-size: 12px;
  }
  .issue-item p {
    color: var(--amber-text-secondary);
    font-size: 11.5px;
    margin-top: 3px;
  }
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: var(--backdrop-bg);
    backdrop-filter: blur(8px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 16px;
    overflow-y: auto;
  }
  .modal-card {
    width: min(680px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow-y: auto;
  }
</style>

<NativeShares {status} {discovery} nativeOnly />
