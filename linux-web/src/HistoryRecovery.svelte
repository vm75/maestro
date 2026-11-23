<script lang="ts">
  import { invokeTool, type Job } from './lib/api'

  let { target, revision, writable, onchanged }: {
    target: string
    revision: string | null
    writable: boolean
    onchanged: () => Promise<void>
  } = $props()
  interface Review {
    plan: { desiredRevision: string; restartRequired: boolean; changes: Array<{resource: string; operation: string}> }
    unavailable: string[]
  }
  let mode = $state<'now' | 'next_start'>('now')
  let content = $state('')
  let valid = $state(false)
  let reviewed = $state<{key: string; result: Review} | null>(null)
  let busy = $state(false)
  let error = $state('')
  let jobStatus = $state('')
  const key = $derived(JSON.stringify([target, revision, mode]))
  const review = $derived(reviewed?.key === key ? reviewed.result : null)

  async function inspect() {
    if (busy || !writable || !revision) return
    busy = true
    error = ''
    content = ''
    reviewed = null
    const requestedKey = key
    const requestedTarget = target
    try {
      const snapshot = await invokeTool<{content: string; valid: boolean}>('managed_config_history_read', {target: requestedTarget})
      if (key !== requestedKey) return
      content = snapshot.content
      valid = snapshot.valid
      if (valid) {
        const result = await invokeTool<Review>('managed_config_revert_plan', {target: requestedTarget, revision})
        if (key === requestedKey) reviewed = {key: requestedKey, result}
      }
    } catch (failure) {
      error = failure instanceof Error ? failure.message : 'Could not review historical configuration.'
    } finally { busy = false }
  }

  async function restore() {
    if (!review || busy || !writable || review.unavailable.length) return
    if (!window.confirm(mode === 'next_start' ? 'Restore these definitions for next start? Workload data and client effects are not restored. Running workloads may need a later restart.' : 'Restore this configuration and apply its definitions now? Running workloads may be interrupted. Workload data and client effects are not restored.')) return
    busy = true
    error = ''
    try {
      const started = await invokeTool<{job: Job}>('managed_config_revert', {target, revision, mode, confirm: true})
      let job = started.job
      jobStatus = 'Revert job: ' + job.id + ' (' + job.state + ')'
      while (job.state === 'running') {
        await new Promise(resolve => setTimeout(resolve, 1000))
        job = await invokeTool<Job>('jobs_get', {id: job.id})
        jobStatus = 'Revert job: ' + job.id + ' (' + job.state + ')'
      }
      if (job.state === 'failed') error = job.error ?? 'Revert failed. Inspect the pending operation before retrying.'
      if (job.state === 'cancelled') error = 'Revert was cancelled.'
      reviewed = null
      await onchanged()
    } catch (failure) {
      error = failure instanceof Error ? failure.message : 'Could not restore historical configuration.'
    } finally { busy = false }
  }
</script>

<div class="history-recovery">
  <label>Restore mode <select bind:value={mode} disabled={busy}><option value="now">Now</option><option value="next_start">Next start</option></select></label>
  <button type="button" disabled={busy || !writable || !revision} onclick={inspect}>
    {busy ? 'Working…' : 'Inspect and review revision'}
  </button>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if jobStatus}<p role="status">{jobStatus}</p>{/if}
  {#if content}
    <details><summary>Private historical YAML</summary><pre>{content}</pre></details>
    {#if !valid}<p role="status">This revision contains invalid configuration. Inspect it for repair; it cannot be applied.</p>{/if}
  {/if}
  {#if review}
    <p>Restoring these definitions preserves workload data. Completed runtime and client effects cannot be reversed.</p>
    {#if review.plan.restartRequired}<p role="status">Restart Maestro after saving these server settings.</p>{/if}
    <ul>{#each review.plan.changes as change}<li>{change.resource}: {change.operation}</li>{/each}</ul>
    {#if review.unavailable.length}<p role="status">Unavailable native changes: {review.unavailable.join(', ')}</p>{/if}
    <button type="button" disabled={busy || !writable || review.unavailable.length > 0} onclick={restore}>Restore reviewed configuration</button>
  {/if}
</div>

<style>
  .history-recovery { margin: .5rem 0 1rem; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 24rem; overflow-y: auto; }
</style>
