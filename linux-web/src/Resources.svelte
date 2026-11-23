<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool } from './lib/api'

  interface Snapshot {
    valid: boolean
    at: string | null
    intervalSeconds: number
    cpu: { usage: number | null }
    memory: { total: number; used: number }
    load: number[] | null
    networks: Array<{ name: string; rxBytesPerSecond: number | null; txBytesPerSecond: number | null }>
    history: Array<{ at: string; cpu: number | null; memoryPercent: number }>
    disks: Array<{ mount: string; filesystem: string; used: number; total: number }>
    diskIO: Array<{ name: string; readBytesPerSecond: number | null; writeBytesPerSecond: number | null }>
    temperatures: Array<{ name: string; celsius: number }>
    warnings: string[]
    lastError?: string
  }

  let snapshot = $state<Snapshot | null>(null)
  let error = $state('')
  const percent = (value: number | null) => value === null ? '—' : `${value.toFixed(1)}%`
  const memory = (bytes: number) => `${(bytes / 1024 ** 3).toFixed(2)} GiB`
  const rate = (value: number | null) => value === null ? '—' : `${(value / 1024).toFixed(1)} KiB/s`

  onMount(() => {
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    async function refresh() {
      try {
        const current = await invokeTool<Snapshot>('monitor_snapshot', {}, controller.signal)
        if (!controller.signal.aborted) {
          snapshot = current
          error = current.lastError ?? ''
        }
      } catch (failure) {
        if (!controller.signal.aborted) {
          error = failure instanceof Error ? failure.message : 'Could not load resource metrics.'
        }
      } finally {
        if (!controller.signal.aborted) {
          timer = setTimeout(refresh, (snapshot?.intervalSeconds ?? 3) * 1000)
        }
      }
    }
    void refresh()
    return () => {
      controller.abort()
      clearTimeout(timer)
    }
  })
</script>

<section class="panel" aria-labelledby="resources-title">
  <h2 id="resources-title">Resources</h2>
  {#if error}<p role="status">{error}</p>{/if}
  {#if snapshot?.valid}
    <dl>
      <dt>CPU</dt><dd>{percent(snapshot.cpu.usage)}</dd>
      <dt>Memory</dt><dd>{memory(snapshot.memory.used)} of {memory(snapshot.memory.total)}</dd>
      {#if snapshot.load}<dt>Load average</dt><dd>{snapshot.load.map(value => value.toFixed(2)).join(' / ')}</dd>{/if}
      {#each snapshot.networks as nic (nic.name)}
        <dt>{nic.name}</dt><dd>{rate(nic.rxBytesPerSecond)} received · {rate(nic.txBytesPerSecond)} sent</dd>
      {/each}
      {#if snapshot.at}<dt>Sample time</dt><dd>{new Date(snapshot.at).toLocaleTimeString()}</dd>{/if}
    </dl>
    {#if snapshot.disks.length || snapshot.diskIO.length || snapshot.temperatures.length}
      <details>
        <summary>Disks and temperatures</summary>
        <dl>
          {#each snapshot.disks as disk}
            <dt>{disk.mount} ({disk.filesystem})</dt><dd>{memory(disk.used)} of {memory(disk.total)}</dd>
          {/each}
          {#each snapshot.diskIO as disk (disk.name)}
            <dt>{disk.name}</dt><dd>{rate(disk.readBytesPerSecond)} read · {rate(disk.writeBytesPerSecond)} written</dd>
          {/each}
          {#each snapshot.temperatures as sensor}
            <dt>{sensor.name}</dt><dd>{sensor.celsius.toFixed(1)} °C</dd>
          {/each}
        </dl>
      </details>
    {/if}
    {#each snapshot.warnings as warning}<p role="status">{warning}</p>{/each}
    {#if snapshot.history.length > 1}
      <div class="history" aria-label="Recent memory utilization" role="img">
        {#each snapshot.history as point (point.at)}
          <span style:height={`${point.memoryPercent}%`} title={percent(point.memoryPercent)}></span>
        {/each}
      </div>
    {/if}
  {:else if !error}
    <p role="status">Waiting for a resource sample…</p>
  {/if}
</section>

<style>
  details { margin: 1rem 0; }
  summary { cursor: pointer; margin-bottom: .6rem; }
  .history { display: flex; align-items: end; gap: 2px; height: 2.5rem; margin-top: 1rem; }
  .history span { flex: 1; max-width: 1rem; background: var(--accent); border-radius: 2px 2px 0 0; }
</style>
