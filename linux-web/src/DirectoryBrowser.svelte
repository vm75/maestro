<script lang="ts">
  import { invokeTool } from './lib/api'
  interface Listing { path: string; parent: string; directories: string[]; files: string[] }
  let path = $state('/')
  let listing = $state<Listing | null>(null)
  let loading = $state(false)
  let error = $state('')
  async function browse(target: string) {
    if (loading) return
    loading = true
    error = ''
    try {
      listing = await invokeTool<Listing>('managed_directory_browse', {path: target})
      path = listing.path
    } catch (failure) {
      listing = null
      error = failure instanceof Error ? failure.message : 'Could not read directory.'
    } finally { loading = false }
  }
</script>

<section class="panel" aria-labelledby="directory-title">
  <h2 id="directory-title">Directory paths</h2>
  <p>Inspect paths accessible to the server account. Hidden names and file contents are excluded. Listings show up to 256 entries.</p>
  <form onsubmit={(event) => { event.preventDefault(); void browse(path) }}>
    <label for="browse-path">Directory</label>
    <input id="browse-path" bind:value={path} maxlength="4096" disabled={loading} />
    <button type="submit" disabled={loading}>{loading ? 'Reading…' : 'Browse'}</button>
  </form>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if listing}
    <p>Current path: <code>{listing.path}</code></p>
    <button type="button" disabled={loading || listing.parent === listing.path} onclick={() => void browse(listing!.parent)}>Parent directory</button>
    <ul>
      {#each listing.directories as name}
        <li><button type="button" disabled={loading} onclick={() => void browse(listing!.path.replace(/\/$/, '') + '/' + name)}>{name}/</button></li>
      {/each}
      {#each listing.files as name}<li>{name}</li>{/each}
    </ul>
    {#if listing.directories.length + listing.files.length === 0}<p>No visible entries.</p>{/if}
  {/if}
</section>

<style>
  form { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  input { flex: 1; min-width: 10rem; font: inherit; padding: .5rem; }
  code, li { overflow-wrap: anywhere; }
  ul { max-height: 20rem; overflow: auto; }
</style>
