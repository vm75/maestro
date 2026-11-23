<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, type Job } from './lib/api'
  let { writable }: { writable: boolean } = $props()
  let content = $state('')
  let revision = $state('missing')
  let busy = $state(false)
  let error = $state('')
  let message = $state('')
  let mode = $state<'now' | 'next_start'>('now')
  interface Preview { plan: {restartRequired: boolean; changes: Array<{resource:string; operation:string}>}; unavailable: string[] }
  let reviewed = $state<{content:string; revision:string; preview:Preview} | null>(null)
  const preview = $derived(reviewed?.content === content && reviewed?.revision === revision ? reviewed.preview : null)
  async function read() {
    busy = true; error = ''
    try {
      const current = await invokeTool<{yaml:string; revision:string|null}>('workloads_get')
      content = current.yaml; revision = current.revision ?? 'missing'; reviewed = null
    } catch (failure) { error = failure instanceof Error ? failure.message : 'Could not read configuration.' }
    finally { busy = false }
  }
  async function review() {
    busy = true; error = ''; reviewed = null
    const candidate = content; const base = revision
    try {
      const result = await invokeTool<Preview>('managed_config_plan', {content:candidate, revision:base})
      reviewed = {content:candidate, revision:base, preview:result}
    } catch (failure) { error = failure instanceof Error ? failure.message : 'Could not preview configuration.' }
    finally { busy = false }
  }
  async function save() {
    if (!preview || !writable || busy || !window.confirm('Save this reviewed configuration? Native definitions may change.')) return
    busy = true; error = ''; message = ''
    try {
      const started = await invokeTool<{job:Job}>('managed_config_save', {content,revision,mode,confirm:true})
      let job = started.job
      while (job.state === 'running') {
        await new Promise(resolve => setTimeout(resolve, 1000))
        job = await invokeTool<Job>('jobs_get', {id:job.id})
      }
      if (job.state !== 'succeeded') throw new Error(job.error ?? 'Configuration save did not complete.')
      const result = job.result as {revision:string; restartRequired:boolean}
      revision = result.revision; reviewed = null
      message = result.restartRequired ? 'Configuration saved. Restart Maestro before further managed operations.' : mode === 'next_start' ? 'Configuration published. Running workloads may need a later restart to use the new definitions.' : 'Configuration saved.'
    } catch (failure) { error = failure instanceof Error ? failure.message : 'Could not save configuration.' }
    finally { busy = false }
  }
  onMount(() => { void read() })
</script>
<section class="panel" aria-labelledby="yaml-editor-title">
  <h2 id="yaml-editor-title">Authoritative YAML</h2>
  <p>Save server settings separately from resource changes. Changed server settings take effect after restarting Maestro.</p>
  <label for="yaml-document">Private configuration (may contain credential hashes)</label>
  <textarea id="yaml-document" bind:value={content} disabled={busy} rows="14" spellcheck="false"></textarea>
  <div class="controls">
    <button type="button" onclick={review} disabled={busy || !content}>Review changes</button>
    <label>Apply mode <select bind:value={mode} disabled={busy}><option value="now">Now</option><option value="next_start">Next start</option></select></label>
    <button type="button" onclick={save} disabled={busy || !writable || !preview || !!preview.unavailable.length}>Save reviewed configuration</button>
    <button type="button" onclick={read} disabled={busy}>Reload</button>
  </div>
  {#if preview}
    {#if preview.plan.restartRequired}<p role="status">Maestro restart required after saving.</p>{/if}
    {#if mode === 'next_start' && preview.plan.changes.length > 0}<p role="status">Publish definitions without starting or restarting workloads. Existing running workloads may need a later restart. Provider readiness and ownership checks still apply.</p>{/if}
    <ul>{#each preview.plan.changes as change}<li>{change.resource}: {change.operation}</li>{/each}</ul>
    {#if preview.unavailable.length}<p role="alert">Unavailable resources: {preview.unavailable.join(', ')}</p>{/if}
  {/if}
  {#if message}<p role="status">{message}</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
<style>
  textarea { width: 100%; box-sizing: border-box; font-family: monospace; background: var(--panel); color: var(--text); padding: .75rem; border: 1px solid var(--border); }
  .controls { display:flex; gap:.5rem; flex-wrap:wrap; align-items:center; margin-top:.5rem; }
</style>
