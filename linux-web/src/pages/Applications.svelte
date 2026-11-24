<script lang="ts">
  import { onMount } from 'svelte'
  import DefinitionEditor from '../lib/DefinitionEditor.svelte'
  import NativeWorkloads from '../lib/NativeWorkloads.svelte'
  import Icon from '../lib/Icon.svelte'
  import ConfirmDialog from '../lib/ConfirmDialog.svelte'
  import type { ConfigurationIssue, ConfigurationHealth } from '../lib/managed'
  import { invokeTool, startJob, type Job } from '../lib/api'
  let { allowWrites }: { allowWrites: boolean } = $props()
  type Issue = ConfigurationIssue
  interface Service { id: string; name: string; kind: string; backend: string; enabled: boolean; state: string; registered: boolean; configuration: string; issues: Issue[]; members?: { name: string; state: string; ports?: string[] }[] }
  interface Inventory { configured: boolean; valid?: boolean; error?: string; services: Service[]; health?: ConfigurationHealth }
  let inventory = $state<Inventory | null>(null)
  let error = $state('')
  let loading = $state(false)
  let query = $state('')
  type EditorState = {
    id: string
    initialContainer?: string
    initialRemoval?: boolean
    singleContainer?: boolean
    initialKind?: 'container' | 'vm'
    initialEngine?: 'podman' | 'docker'
    initialBackend?: 'quadlet' | 'compose' | 'libvirt' | 'incus'
  }
  let editor = $state<EditorState | null>(null)
  let editorDialog = $state<HTMLDialogElement>()
  let feedback = $state('')
  let activeJob = $state<string | null>(null)
  let pending = $state<{ service: Service; action: string; member?: string } | null>(null)
  let logService = $state<Service | null>(null)
  let logMember = $state<string | undefined>(undefined)
  let submitting = $state(false)
  let logTail = $state(200)
  let logLoading = $state(false)
  let logError = $state('')
  let logs = $state<{ text: string; truncated: boolean; source: string } | null>(null)
  let logController: AbortController | undefined
  let controller: AbortController | undefined
  let stopped = false
  const filtered = $derived(inventory?.services.filter(s => `${s.id} ${s.name} ${s.backend} ${s.members?.map(member => member.name).join(' ') ?? ''}`.toLowerCase().includes(query.toLowerCase())) ?? [])
  const issues = $derived(inventory?.health?.issues ?? [])
  const blocked = $derived(issues.filter(i => i.severity !== 'warning'))

  function openEditor(
    kind: 'container' | 'vm',
    engine?: 'podman' | 'docker',
    backend?: 'quadlet' | 'compose' | 'libvirt' | 'incus',
    singleContainer = false
  ) {
    editor = {
      id: '',
      initialKind: kind,
      initialEngine: engine,
      initialBackend: backend,
      singleContainer
    }
  }

  async function load(recheck = false) {
    controller?.abort(); controller = new AbortController(); const signal = controller.signal
    loading = true; error = ''
    try {
      if (recheck && inventory?.configured && inventory.valid) await invokeTool('managed_config_check', {}, signal)
      const result = await invokeTool<Inventory>('managed_services', {}, signal)
      if (!stopped && !signal.aborted) inventory = result
    } catch (e) { if (!signal.aborted) error = e instanceof Error ? e.message : 'Could not read applications' }
    finally { if (!signal.aborted) loading = false }
  }
  function showModal(dialog: HTMLDialogElement) {
    dialog.showModal()
  }

  function closeLogs() {
    logController?.abort()
    logService = null
  }

  const actionLabels: Record<string, string> = {
    start: 'Start', stop: 'Stop', restart: 'Restart',
    enable: 'Enable boot startup', disable: 'Disable boot startup', pull: 'Pull latest image'
  }

  function needsAttention(state: string) {
    return /error|failed|partial|missing|unknown|dead/i.test(state)
  }

  async function readLogs(service: Service, member?: string) {
    logController?.abort(); logController = new AbortController()
    const signal = logController.signal
    logService = service; logMember = member; logs = null; logError = ''; logLoading = true
    try {
      const result = await invokeTool<{ text: string; truncated: boolean; source: string }>('managed_service_logs', { service: service.id, tail: logTail, ...(member ? { container: member } : {}) }, signal)
      if (!stopped && !signal.aborted && logService?.id === service.id) logs = result
    } catch (e) {
      if (!signal.aborted) logError = e instanceof Error ? e.message : 'Could not read application logs'
    } finally { if (!signal.aborted) logLoading = false }
  }
  async function perform(service: Service, action: string, member?: string) {
    if (submitting || activeJob !== null) return
    submitting = true; error = ''; pending = null
    try {
      const job = member
        ? await startJob('managed_container_action', { service: service.id, container: member, action, confirm: true })
        : await startJob(`managed_service_${action}`, { service: service.id, ...(action === 'start' ? {} : { confirm: true }) })
      activeJob = job.id
      feedback = 'Operation started. Follow its progress in Activity.'
    } catch (e) {
      error = e instanceof Error ? e.message : 'Could not start operation'
    } finally {
      submitting = false
    }
  }
  onMount(() => {
    const finished = (event: Event) => { const job = (event as CustomEvent<Job>).detail; if (job.id !== activeJob) return; activeJob = null; feedback = job.state === 'succeeded' ? 'Operation completed.' : job.error || `Operation ${job.state}.`; void load() }
    window.addEventListener('maestro:job-finished', finished)
    void load()
    return () => { stopped = true; controller?.abort(); logController?.abort(); window.removeEventListener('maestro:job-finished', finished) }
  })
</script>

<div class="toolbar">
  <div class="define-actions" role="group" aria-label="Define applications">
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('container', 'podman', 'quadlet', true)}>Single Podman container</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('container', 'docker', 'compose', true)}>Single Docker container</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('container', 'podman', 'compose')}>Podman Compose</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('container', 'podman', 'quadlet')}>Podman Quadlet</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('container', 'docker', 'compose')}>Docker Compose</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('vm', undefined, 'incus')}>Incus VM</button>
    <button class="small-button primary" disabled={!allowWrites || activeJob !== null} onclick={() => openEditor('vm', undefined, 'libvirt')}>KVM/Libvirt VM</button>
  </div>
  <div class="search-wrap">
    <label class="sr-only" for="app-search">Find an application</label>
    <input id="app-search" type="search" bind:value={query} placeholder="Find an application by name or backend…" />
  </div>
  <button disabled={loading} onclick={() => load(true)}>{loading ? 'Checking…' : 'Check configuration'}</button>
</div>

<NativeWorkloads {query} {allowWrites} />

{#if editor}
  <dialog class="modal-card" aria-label="Application definition" bind:this={editorDialog} use:showModal onclose={() => { editor = null }}>
    {#key `${editor.id}-${editor.initialKind}-${editor.initialEngine}-${editor.initialBackend}`}
      <DefinitionEditor
        kind="service"
        resourceID={editor.id}
        singleContainer={editor.singleContainer}
        initialContainer={editor.initialContainer}
        initialRemoval={editor.initialRemoval}
        initialKind={editor.initialKind}
        initialEngine={editor.initialEngine}
        initialBackend={editor.initialBackend}
        onClose={() => editorDialog?.close()}
        onSaved={(id) => {
          feedback = editor?.initialRemoval ? 'Deletion started. Follow its progress in Activity.' : 'Definition save started. Follow its validation and apply progress in Activity.'
          activeJob = id
          editorDialog?.close()
        }}
      />
    {/key}
  </dialog>
{/if}
{#if error}<div class="notice error" role="alert"><p>{error}</p><button onclick={() => load(true)}>Retry</button></div>{/if}
{#if feedback}<div class="job-feedback" role="status">{feedback}</div>{/if}
{#if inventory?.error}<section class="panel health-alert" role="alert"><h2>Configuration needs attention</h2><p>{inventory.error}</p></section>{/if}
{#if blocked.length}<section class="panel health-alert" role="alert"><h2>Configuration needs attention</h2><p>Some applications have startup requirements to resolve. Restore missing data and repair host dependencies before starting them.</p></section>{/if}
{#if inventory?.health}<p class="resource-hint">Files checked {new Date(inventory.health.checkedAt).toLocaleString()}. {#if inventory.health.runtimeCheckedAt}Native requirements checked {new Date(inventory.health.runtimeCheckedAt).toLocaleString()}.{/if}</p>{/if}
{#if !allowWrites}<p class="resource-hint">Management writes are disabled. Status and configuration checks remain available.</p>{/if}

{#if inventory && !inventory.error && !inventory.services.length}
  <section class="panel empty-state">
    <h2>No managed applications yet</h2>
    <p>No applications have been defined yet. Choose an application or virtual machine type to configure and create workloads. Configure storage and host bindings in Configuration first.</p>
    <div class="empty-define-actions">
      <button class="primary" disabled={!allowWrites} onclick={() => openEditor('container', 'podman', 'compose')}>Podman Compose</button>
      <button class="primary" disabled={!allowWrites} onclick={() => openEditor('container', 'podman', 'quadlet')}>Podman Quadlet</button>
      <button class="primary" disabled={!allowWrites} onclick={() => openEditor('container', 'docker', 'compose')}>Docker Compose</button>
      <button class="primary" disabled={!allowWrites} onclick={() => openEditor('vm', undefined, 'incus')}>Incus VM</button>
      <button class="primary" disabled={!allowWrites} onclick={() => openEditor('vm', undefined, 'libvirt')}>KVM/Libvirt VM</button>
    </div>
  </section>
{:else if inventory && !inventory.error && inventory.services.length && !filtered.length}
  <section class="panel empty-state">
    <h2>No matching applications</h2>
    <p>Try a different search term or clear the filter.</p>
    <button onclick={() => { query = '' }}>Clear search</button>
  </section>
{/if}

{#snippet status(state: string)}
  <span class="project-state" class:running={state === 'running'} class:attention={needsAttention(state)}>
    {state.replaceAll('_', ' ')}
  </span>
{/snippet}

{#if filtered.length}
  <div class="panel applications">
  <div class="application-row list-head" aria-hidden="true">
    <span>Name</span><span>Type</span><span>Status</span><span>Published ports</span><span>Actions</span>
  </div>
  <ul aria-label="Managed applications">
    {#each filtered as service (service.id)}
      {@const name = service.name || service.id}
      {@const kind = service.backend.endsWith('/compose') ? 'Compose project' : service.kind === 'vm' ? 'VM' : 'Quadlet group'}
      {@const label = `${name} (${kind})`}
      {@const canEdit = allowWrites && activeJob === null && !submitting}
      {@const canReadLogs = service.kind === 'container' || service.backend === 'incus'}
      <li>
        <div class="application-row">
          <div class="row-name">
            <Icon name={service.kind === 'vm' ? 'machines' : 'applications'} size={16} />
            <strong>{name}</strong>
          </div>
          <div class="row-type">{kind}<small>{service.backend}</small></div>
          <div class="row-status">
            {@render status(service.state)}
            <span class="project-state">{service.registered ? 'Registered' : 'Not registered'}</span>
            {#if service.configuration === 'restart_required'}
              <span class="project-state attention">Restart required</span>
            {/if}
            {#if service.issues.length}
              <span class="project-state attention">{service.issues.some(issue => issue.severity !== 'warning') ? 'Configuration error' : 'Configuration warning'}</span>
            {/if}
          </div>
          <div class="row-ports">{service.kind === 'vm' ? 'Unmanaged' : '—'}</div>
          <div class="row-actions">
            {#each ['start', 'stop', 'restart', service.enabled ? 'disable' : 'enable'] as action}
              <button type="button" title={actionLabels[action]} aria-label={`${actionLabels[action]} · ${label}`}
                disabled={!canEdit || loading || (['start', 'restart', 'enable'].includes(action) && service.issues.some(i => i.severity !== 'warning' && (action === 'enable' || i.field !== 'autostart')))}
                onclick={() => { if (action === 'start') void perform(service, action); else pending = { service, action } }}>
                <Icon name={action} size={16} />
              </button>
            {/each}
            <button type="button" title="Edit definition" aria-label={`Edit definition · ${label}`} disabled={!canEdit} onclick={() => { editor = { id: service.id } }}>
              <Icon name="edit" size={16} />
            </button>
            <button type="button" title={canReadLogs ? 'View logs' : 'Logs unavailable for this backend'} aria-label={`View logs · ${label}`} disabled={!canReadLogs}
              onclick={() => { logTail = 200; void readLogs(service) }}>
              <Icon name="logs" size={16} />
            </button>
            <button type="button" title="Delete application" aria-label={`Delete · ${label}`} disabled={!canEdit}
              onclick={() => { editor = { id: service.id, initialRemoval: true } }}>
              <Icon name="trash" size={16} />
            </button>
          </div>
        </div>
        {#if service.members?.length}
          <ul class="members" aria-label={`Containers in ${name}`}>
            {#each service.members as member (member.name)}
              <li class="application-row member">
                <div class="row-name">
                  <Icon name="containers" size={15} />
                  <strong>{member.name}</strong>
                </div>
                <div class="row-type">Container</div>
                <div class="row-status">{@render status(member.state)}</div>
                <div class="row-ports">
                  {#each member.ports ?? [] as port}<code>{port}</code>{:else}<span>—</span>{/each}
                </div>
                <div class="row-actions">
                  {#each ['start', 'stop', 'restart', 'pull'] as action}
                    <button type="button" title={actionLabels[action]} aria-label={`${actionLabels[action]} · ${name}/${member.name}`}
                      disabled={!canEdit || loading || (['start', 'restart'].includes(action) && service.issues.some(i => i.severity !== 'warning' && i.field !== 'autostart'))}
                      onclick={() => { if (action === 'start') void perform(service, action, member.name); else pending = { service, action, member: member.name } }}>
                      <Icon name={action} size={16} />
                    </button>
                  {/each}
                  <button type="button" title="Edit container" aria-label={`Edit container ${member.name} in ${name}`} disabled={!canEdit}
                    onclick={() => { editor = { id: service.id, initialContainer: member.name } }}>
                    <Icon name="edit" size={16} />
                  </button>
                  <button type="button" title="View container logs" aria-label={`View logs · ${name}/${member.name}`}
                    onclick={() => { logTail = 200; void readLogs(service, member.name) }}>
                    <Icon name="logs" size={16} />
                  </button>
                  <button type="button" title={service.members.length <= 1 ? 'Delete the application to remove its last container' : 'Delete container'}
                    aria-label={`Delete · ${name}/${member.name}`} disabled={!canEdit || service.members.length <= 1}
                    onclick={() => { editor = { id: service.id, initialContainer: member.name, initialRemoval: true } }}>
                    <Icon name="trash" size={16} />
                  </button>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>
</div>
{/if}

{#if logService}
  <dialog class="modal-card panel logs-dialog" aria-labelledby="application-logs-heading" use:showModal onclose={closeLogs}>
    <header>
      <h2 id="application-logs-heading">{logService.backend === 'incus' ? 'VM diagnostics' : 'Application logs'} · {logService.name || logService.id}{logMember ? ` / ${logMember}` : ''}</h2>
      <button type="button" title="Close logs" aria-label="Close logs" onclick={(event) => event.currentTarget.closest('dialog')?.close()}><Icon name="close" size={16} /></button>
    </header>
    <div class="log-controls">
      <label for="application-log-tail">Log entries</label>
      <select id="application-log-tail" bind:value={logTail}>
        {#each [100, 200, 500, 1000, 2000] as count}<option value={count}>{count}</option>{/each}
      </select>
      <button type="button" disabled={logLoading} onclick={() => { if (logService) void readLogs(logService, logMember) }}>Refresh logs</button>
    </div>
    {#if logLoading}<p role="status">Reading logs…</p>{/if}
    {#if logError}<p class="form-error" role="alert">{logError}</p>{/if}
    {#if logs}
      <pre class="project-log-output">{logs.text || 'No visible log entries.'}</pre>
      {#if logs.truncated}<p class="resource-hint">Showing only the most recent 128 KiB of output.</p>{/if}
    {/if}
  </dialog>
{/if}

{#if issues.length}
  <details class="panel all-issues">
    <summary>All configuration issues ({issues.length})</summary>
    <ul class="issues">
      {#each issues as issue}
        <li class="issue-item">
          <strong>{issue.resource}: {issue.message}</strong>
          <p>{issue.remedy}</p>
        </li>
      {/each}
    </ul>
  </details>
{/if}

<ConfirmDialog
  open={pending !== null}
  title={pending ? `${actionLabels[pending.action]} · ${pending.service.name || pending.service.id}${pending.member ? ` / ${pending.member}` : ''}` : ''}
  description={pending?.action === 'pull' ? 'Download the configured image tag or digest without restarting the container.' : pending?.action === 'enable' || pending?.action === 'disable' ? 'Change automatic startup at boot.' : 'This interrupts the selected workload.'}
  label="Confirm"
  busy={submitting || activeJob !== null}
  onConfirm={() => { if (pending) void perform(pending.service, pending.action, pending.member) }}
  onClose={() => { pending = null }}
/>

<style>
  .toolbar {
    display: flex;
    gap: 12px;
    align-items: center;
    margin-bottom: 24px;
    flex-wrap: wrap;
  }
  .define-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
  }
  .empty-define-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 16px;
  }
  .search-wrap {
    flex: 1;
    min-width: 180px;
  }
  .modal-card {
    width: min(860px, calc(100vw - 32px));
    max-height: calc(100dvh - 48px);
    margin: auto;
    padding: 0;
    overflow: auto;
    color: var(--text);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .modal-card::backdrop { background: rgb(5 9 15 / 75%); }
  .logs-dialog { padding: 16px; }
  .logs-dialog header, .log-controls {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin-bottom: 12px;
  }
  .logs-dialog header { justify-content: space-between; }
  .applications, .applications > ul, .members { list-style: none; margin: 0; padding: 0; }
  .applications > ul > li + li { border-top: 1px solid var(--border); }
  .application-row {
    display: grid;
    grid-template-columns: minmax(120px, 1.15fr) minmax(90px, .7fr) minmax(100px, .85fr) minmax(115px, 1fr) 253px;
    align-items: center;
    gap: 10px;
    padding: 8px 12px;
  }
  .member { background: var(--panel-subtle); border-top: 1px solid var(--border-subtle); }
  .member .row-name { padding-left: 18px; }
  .row-name, .row-status, .row-actions { display: flex; align-items: center; gap: 6px; }
  .row-name { overflow-wrap: anywhere; }
  .list-head { font-weight: 600; color: var(--muted); border-bottom: 1px solid var(--border); }
  .list-head span:last-child { text-align: right; }
  .row-type, .row-ports { overflow-wrap: anywhere; font-size: 12px; color: var(--muted); }
  .row-type small, .row-ports code { display: block; }
  .row-ports code { color: var(--text); }
  .row-status, .row-actions { flex-wrap: wrap; }
  .row-status { text-transform: capitalize; }
  .row-actions { justify-content: flex-end; }
  .row-actions button {
    display: inline-grid;
    place-items: center;
    width: 31px;
    min-height: 31px;
    padding: 0;
  }
  .applications { overflow-x: auto; }
  .application-row { min-width: 725px; }
  .issues {
    list-style: none;
    padding: 0;
    margin: 16px 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .issue-item {
    background: var(--amber-dim);
    border: 1px solid var(--amber-border);
    border-radius: var(--radius-sm);
    padding: 12px 16px;
  }
  .issue-item strong {
    color: var(--amber-text);
    font-size: 12.5px;
  }
  .issue-item p {
    color: var(--amber-text-secondary);
    font-size: 12px;
    margin-top: 4px;
  }
  .health-alert {
    border-left: 4px solid var(--amber);
    background: var(--amber-dim);
    border-color: var(--amber-border);
    margin-bottom: 20px;
  }
  .health-alert h2 {
    color: var(--amber-text);
    font-size: 16px;
    margin-bottom: 6px;
  }
  .health-alert p {
    color: var(--amber-text-secondary);
  }
  .all-issues {
    margin-top: 24px;
  }
</style>
