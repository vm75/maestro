<script lang="ts">
  import NginxFields from './NginxFields.svelte'
  import {accessNames,listenerNames,accessValue,setAccess,setField,policyFields,type GlobalSettings,type Settings} from './nginx-config'
  let { value = $bindable({}), global, kind = 'domain', defaults = false, disabled = false }:
    {value: Settings; global: GlobalSettings; kind?: string; defaults?: boolean; disabled?: boolean} = $props()
  const inherited = $derived(defaults ? {} : global.site_defaults)
  const fields = $derived(policyFields.filter(f => !f.domainOnly || kind === 'domain').map(f => f.key === 'limit_policy' ? {...f, options:['default',...Object.keys(global.limit_policies)]} : f))
  let routeText = $state('')
  let routeError = $state('')
  let rawOpen = $state(false)
  let raw = $state('')
  let rawError = $state('')
  let path = $state('')
  let match = $state('prefix')
  let access = $state('deny')
  let editingRoute = $state<number | null>(null)
  function editRoute(route: Settings, i: number) {
    editingRoute = i; path = String(route.path ?? ''); match = String(route.match ?? 'prefix'); access = String(route.access_policy ?? (route.enable_lanonly === undefined ? '' : route.enable_lanonly ? 'legacy' : 'public'))
  }
  function resetRoute() { editingRoute = null; path = ''; match = 'prefix'; access = 'deny' }
  const routes = $derived(Array.isArray(value.routes) ? value.routes as Settings[] : [])
  const displayedRoutes = $derived(value.routes === undefined && Array.isArray(inherited.routes) ? inherited.routes as Settings[] : routes)
  function setRoutes(next: Settings[] | undefined) { value = setField(value,'routes',next); routeText = next === undefined ? '' : JSON.stringify(next,null,2); routeError = ''; resetRoute() }
  function editRoutes() {
    try {
      const parsed: unknown = JSON.parse(routeText)
      if (!Array.isArray(parsed) || parsed.some(r => !r || typeof r !== 'object' || Array.isArray(r))) throw new Error('Routes must be an array of objects')
      setRoutes(parsed as Settings[])
    } catch(e) { routeError = String(e) }
  }
  function addRoute() {
    if (!path.startsWith('/')) { routeError = 'Path must start with /'; return }
    // On first override preserve inherited routes, rather than silently drop them.
    const current = value.routes === undefined && Array.isArray(inherited.routes) ? inherited.routes as Settings[] : routes
    const original = editingRoute === null ? {} : current[editingRoute] || {}
    if (access === 'deny' && Object.keys(original).some(key => !['path','match','access_policy','enable_lanonly'].includes(key))) {
      routeError = 'Deny routes cannot contain advanced options. Remove them in Advanced route policies before choosing deny.'; return
    }
    const next = {...original,path,match}
    const normalized = setAccess(next,access)
    setRoutes(editingRoute === null ? [...current,normalized] : current.map((route,i) => i === editingRoute ? normalized : route))
  }
  function openRaw() { raw = JSON.stringify(value,null,2); rawError = ''; rawOpen = true }
  function applyRaw() {
    try {
      const parsed: unknown = JSON.parse(raw)
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('Settings must be an object')
      value = parsed as Settings; rawOpen = false; rawError = ''; routeText = JSON.stringify(value.routes ?? [],null,2)
    } catch(e) { rawError = String(e) }
  }
</script>
<div class="policy-grid">
  {#if kind !== 'subfolder'}
    <label>Listener profile<select disabled={disabled} value={String(value.listener_profile ?? '')} onchange={e => value=setField(value,'listener_profile',e.currentTarget.value || undefined)}>
      <option value="">Inherit ({String(inherited.listener_profile ?? 'legacy')})</option>
      {#each listenerNames(global) as name}<option value={name}>{name}</option>{/each}
      {#if value.listener_profile && !listenerNames(global).includes(String(value.listener_profile))}<option value={String(value.listener_profile)}>Missing saved choice: {String(value.listener_profile)}</option>{/if}
    </select></label>
  {:else}<p>Listener inherited from the parent domain.</p>{/if}
  <label>Access policy<select disabled={disabled} value={accessValue(value)} onchange={e => value=setAccess(value,e.currentTarget.value)}>
    <option value="">Inherit ({accessValue(inherited) || 'legacy'})</option>
    {#each accessNames(global) as name}<option value={name}>{name}</option>{/each}
    {#if accessValue(value) && !accessNames(global).includes(accessValue(value))}<option value={accessValue(value)}>Missing saved choice: {accessValue(value)}</option>{/if}
  </select></label>
  <label>Restricted Sessions access policy<select disabled={disabled} value={String(value.restricted_sessions_policy ?? '')} onchange={e => value=setField(value,'restricted_sessions_policy',e.currentTarget.value || undefined)}>
    <option value="">Inherit ({String(inherited.restricted_sessions_policy ?? 'legacy')})</option>
    {#each accessNames(global) as name}<option value={name}>{name}</option>{/each}
  </select></label>
</div>
<NginxFields {fields} bind:value {inherited} {disabled} />
<details>
  <summary>Path access rules and blocked paths</summary>
  <p class="muted">{value.routes === undefined ? 'Inheriting default routes (editing creates an override)' : routes.length + ' explicit route policies'}. Child rules cannot loosen parent access restrictions.</p>
  {#each displayedRoutes as route,i}
    <div class="route-row"><code>{String(route.path ?? '')}</code> · {String(route.match ?? 'prefix')} · {String(route.access_policy ?? 'inherit')}
      <button type="button" disabled={disabled} onclick={() => editRoute(route,i)}>Edit</button>
      <button type="button" disabled={disabled} onclick={() => setRoutes(displayedRoutes.filter((_,j) => j !== i))}>Remove</button>
    </div>
  {/each}
  <div class="policy-grid">
    <label>Path <input bind:value={path} placeholder="/admin" disabled={disabled}/></label>
    <label>Match <select bind:value={match} disabled={disabled}><option value="prefix">Prefix (includes subpaths)</option><option value="exact">Exact</option><option value="regex">Regex</option><option value="regex_i">Case-insensitive regex</option></select></label>
    <label>Access policy <select bind:value={access} disabled={disabled}><option value="">Inherit parent access</option>{#each accessNames(global) as name}<option value={name}>{name}</option>{/each}</select></label>
  </div>
  <button type="button" disabled={disabled} onclick={addRoute}>{editingRoute === null ? 'Add path rule' : 'Update path rule'}</button>
  {#if editingRoute !== null}<button type="button" disabled={disabled} onclick={resetRoute}>Cancel path edit</button>{/if}
  <button type="button" disabled={disabled} onclick={() => setRoutes(undefined)}>Inherit routes</button>
  <button type="button" disabled={disabled} onclick={() => setRoutes([])}>Clear all routes</button>
  <details ontoggle={e => { if (e.currentTarget.open) routeText=JSON.stringify(value.routes ?? inherited.routes ?? [],null,2) }}>
    <summary>Advanced route policies (nested routes, per-route proxy and rate options)</summary>
    <textarea aria-label="Route policies JSON" bind:value={routeText} rows="8" disabled={disabled}></textarea>
    <button type="button" disabled={disabled} onclick={editRoutes}>Apply route JSON to draft</button>
  </details>
  {#if routeError}<p class="form-error" role="alert">{routeError}</p>{/if}
</details>
<details open={rawOpen} ontoggle={e => { if (e.currentTarget.open && !rawOpen) openRaw(); else if (!e.currentTarget.open) rawOpen=false }}>
  <summary>Advanced managed settings JSON</summary>
  <textarea aria-label="Managed settings JSON" bind:value={raw} rows="8" disabled={disabled}></textarea>
  <button type="button" disabled={disabled} onclick={applyRaw}>Apply JSON to draft</button>
  {#if rawError}<p class="form-error" role="alert">{rawError}</p>{/if}
</details>
<style>
  .policy-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px;margin:12px 0}
  details{margin:12px 0}
  textarea{width:100%;font-family:monospace}
  .route-row{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin:6px 0}
</style>
