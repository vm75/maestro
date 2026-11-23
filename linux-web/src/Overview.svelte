<script lang="ts">
  import Resources from './Resources.svelte'
  import Processes from "./Processes.svelte"
  import type { Discovery, Status } from './lib/api'
  let { status, discovery }: { status: Status; discovery: Discovery } = $props()
</script>

<section class="panel" aria-labelledby="server">
  <h2 id="server">{status.hostname}</h2>
  <dl>
    <dt>Version</dt><dd>{status.version}</dd>
    <dt>Started</dt><dd>{new Date(status.startedAt).toLocaleString()}</dd>
    <dt>Management operations</dt><dd>{status.allowWrites ? 'Read and write' : 'Read only'}</dd>
  </dl>
</section>
<section class="panel" aria-labelledby="capabilities">
  <h2 id="capabilities">Capabilities</h2>
  <ul>
    {#each discovery.modules as module (module.id)}
      <li>{module.title}{module.available ? '' : ' (unavailable)'}</li>
    {/each}
  </ul>
</section>

{#if discovery.modules.some(module => module.id === 'monitor' && module.available)}
  <Resources />
  <Processes />
{/if}
