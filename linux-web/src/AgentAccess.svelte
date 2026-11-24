<script lang="ts">
  import { onMount, untrack } from 'svelte'
  import { request, type Discovery, type Status } from './lib/api'

  let { status: _status, discovery: initial }: { status: Status; discovery: Discovery } = $props()
  let current = $state<Discovery>(untrack(() => initial))
  let endpoint = $state('/api/mcp')
  let updating = $state(false)
  let error = $state('')
  let filter = $state('')
  const visible = $derived(current.tools.filter(tool =>
    (tool.name + ' ' + tool.description + ' ' + tool.module).toLowerCase().includes(filter.toLowerCase())
  ))

  async function refresh() {
    updating = true
    error = ''
    try { current = await request<Discovery>('/api/tools') }
    catch (failure) { error = failure instanceof Error ? failure.message : 'Could not read capabilities.' }
    finally { updating = false }
  }

  onMount(() => { endpoint = window.location.origin + '/api/mcp' })
</script>

<section class="panel" aria-labelledby="agent-title">
  <h2 id="agent-title">Agent connections</h2>
  <p>All agent management tools share the HTTP registry's operation schemas, confirmation requirements and server-side authorization. Protected writes require independently authorized helper access; tool visibility alone does not grant permission.</p>
  <dl>
    <dt>Streamable HTTP endpoint (JSON responses)</dt>
    <dd><code>{endpoint}</code></dd>
    <dt>Transport</dt>
    <dd>JSON-RPC 2.0 · supported MCP revisions 2025-11-25, 2025-06-18 and 2025-03-26</dd>
    <dt>Authentication</dt>
    <dd>Use the configured bearer token or an authenticated browser session. Do not put tokens in URLs or logs.</dd>
    <dt>Stdio</dt>
    <dd><code>maestro-mcp-stdio --config /path/to/maestro.toml</code> as the non-root workload account, using the separate Linux release artifact.</dd>
  </dl>
</section>

<section class="panel" aria-labelledby="agent-permissions">
  <div class="controls">
    <h2 id="agent-permissions">Permissions and availability</h2>
    <button type="button" onclick={refresh} disabled={updating}>Recheck</button>
  </div>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if current.permissions}
    <dl>
      <dt>Privileged helper</dt>
      <dd>{current.permissions.helper.status} — {current.permissions.helper.reason}</dd>
      <dt>System service writes</dt>
      <dd>{current.permissions.systemServices.status} — {current.permissions.systemServices.reason}</dd>
      <dt>Package writes</dt>
      <dd>{current.permissions.packageWrites.status} — {current.permissions.packageWrites.reason}</dd>
      {#if current.permissions.sambaPassword}
        <dt>Samba password writes</dt>
        <dd>{current.permissions.sambaPassword.status} — {current.permissions.sambaPassword.reason}</dd>
      {/if}
    </dl>
  {:else}
    <p role="status">Permission details are unavailable. Recheck the connected server.</p>
  {/if}
</section>

<section class="panel" aria-labelledby="agent-tools">
  <h2 id="agent-tools">Available tool contracts ({current.tools.length})</h2>
  <label for="agent-tool-filter">Filter tools</label>
  <input id="agent-tool-filter" type="search" bind:value={filter} placeholder="Search by name or module">
  <div class="tool-list">
    {#each visible as tool (tool.name)}
      <details>
        <summary><code>{tool.name}</code> — {tool.title}
          {tool.readOnly ? '(read-only)' : tool.destructive ? '(destructive)' : '(write)'}
        </summary>
        <p>{tool.description}</p>
        <p>Module: {tool.module}</p>
        <pre>{JSON.stringify(tool.inputSchema, null, 2)}</pre>
      </details>
    {/each}
  </div>
  {#if visible.length === 0}<p>No matching tool contracts.</p>{/if}
</section>

<style>
  .controls { display: flex; align-items: center; gap: .75rem; flex-wrap: wrap; }
  .controls h2 { margin-right: auto; }
  dt { font-weight: 600; }
  dd { margin: .2rem 0 .75rem; overflow-wrap: anywhere; }
  code { overflow-wrap: anywhere; }
  input { font: inherit; background: var(--panel); color: var(--text); padding: .6rem; border: 1px solid var(--border); border-radius: 6px; max-width: 25rem; width: 100%; }
  .tool-list { display: grid; gap: .5rem; margin-top: 1rem; }
  details { padding: .7rem; border: 1px solid var(--border); border-radius: 6px; }
  summary { cursor: pointer; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 20rem; overflow: auto; }
</style>
