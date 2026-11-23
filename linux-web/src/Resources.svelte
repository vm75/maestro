<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool } from './lib/api'
  import Sparkline from './lib/Sparkline.svelte'
  import { bytes, percent, rate, uptime } from './lib/monitor'

  interface Snapshot {
    valid: boolean
    at: string | null
    intervalSeconds: number
    cpu: { usage: number | null }
    memory: { total: number; used: number }
    uptimeSeconds: number | null
    load: number[] | null
    networks: Array<{ name: string; rxBytesPerSecond: number | null; txBytesPerSecond: number | null }>
    history: Array<{ at: string; cpu: number | null; memoryPercent: number; rx: number | null; tx: number | null }>
    disks: Array<{ mount: string; filesystem: string; used: number; total: number }>
    diskIO: Array<{ name: string; readBytesPerSecond: number | null; writeBytesPerSecond: number | null }>
    temperatures: Array<{ name: string; celsius: number }>
    warnings: string[]
    lastError?: string
  }

  let snapshot = $state<Snapshot | null>(null)
  let error = $state('')
  const sampleTimes = $derived(snapshot?.history.map(point => new Date(point.at).getTime()) ?? [])

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
      <dt>Memory</dt><dd>{bytes(snapshot.memory.used)} of {bytes(snapshot.memory.total)}</dd>
      <dt>Uptime</dt><dd>{uptime(snapshot.uptimeSeconds)}</dd>
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
            <dt>{disk.mount} ({disk.filesystem})</dt><dd>{bytes(disk.used)} of {bytes(disk.total)}</dd>
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
      <div class="history">
        <h3>CPU utilization</h3>
        <Sparkline values={snapshot.history.map(point => point.cpu)} times={sampleTimes} windowSeconds={snapshot.intervalSeconds * 60} ceiling={100} label="Recent CPU utilization" />
        <h3>Memory utilization</h3>
        <Sparkline values={snapshot.history.map(point => point.memoryPercent)} times={sampleTimes} windowSeconds={snapshot.intervalSeconds * 60} ceiling={100} label="Recent memory utilization" />
        <h3>Network throughput</h3>
        <Sparkline values={snapshot.history.map(point => point.rx)} secondary={snapshot.history.map(point => point.tx)} times={sampleTimes} windowSeconds={snapshot.intervalSeconds * 60} label="Recent receive and transmit rates" />
      </div>
    {/if}
  {:else if !error}
    <p role="status">Waiting for a resource sample…</p>
  {/if}
</section>

<style>
  details { margin: 1rem 0; }
  summary { cursor: pointer; margin-bottom: .6rem; }
  .history { margin-top: 1rem; }
  .history :global(.sparkline) { display: block; width: 100%; height: 4rem; }
</style>
