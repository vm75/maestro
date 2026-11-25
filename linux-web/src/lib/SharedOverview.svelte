<script lang="ts">
  import { onMount } from 'svelte'
  import { request, invokeTool, type Discovery } from './api'
  import type { SharedDiscovery } from './shared'
  import type { ServiceInventory } from './services'
  let { shared, discovery, onNavigate }: { shared: SharedDiscovery; discovery: Discovery; onNavigate: (page:string)=>void } = $props()
  type Metric = { label:string; value:string; detail:string; target:string }
  let metrics = $state<Record<string,Metric>>({})
  let loading = $state(false)
  let controller: AbortController | undefined
  const ready = (id:string) => shared.modules.some(m=>m.id===id && !['unsupported','disabled'].includes(m.availability))
  const cards = $derived([
    ...(discovery.modules.some(m=>m.id==='systemd') ? [{id:'services',label:'Services',target:'services'}] : []),
    ...(ready('nginx') ? [{id:'nginx',label:'Nginx objects',target:'web'}] : []),
    ...(ready('certificates') ? [{id:'certificates',label:'Certificates',target:'certificates'}] : []),
    ...(ready('ban_shield') ? [{id:'ban_shield',label:'Active bans',target:'security'}] : [])
  ])
  async function refresh() {
    controller?.abort(); const active = new AbortController(); controller=active; loading=true
    const get = <T,>(path:string) => request<T>('/api/shared/'+path,{signal:active.signal})
    const loaders: Record<string,()=>Promise<[string,string]>> = {
      services: async () => { const v=await invokeTool<ServiceInventory>('services_list',{scope:'user'},active.signal); if(!v.available) throw Error(v.reason || 'User services unavailable'); return [v.services.filter(s=>s.active==='active').length+'/'+v.services.length,'user services running'] },
      nginx: async () => { const [s,v]=await Promise.all([get<{running:boolean}>('nginx/status'),get<unknown[]>('nginx/sites')]);return [String(v.length),s.running?'Nginx online':'Nginx offline'] },
      certificates: async () => { const v=await get<{renewal_due:boolean}[]>('certificates'); const due=v.filter(c=>c.renewal_due).length;return [String(v.length),due?due+' need attention':'Renewals healthy'] },
      ban_shield: async () => { const {snapshot:s}=await get<{snapshot:{active_ban_count:number;banned_subnets:unknown[];banned_ips:unknown[]}}>('firewall?include_bans=false');return [String(s.active_ban_count),s.banned_subnets.length+' subnet · '+s.banned_ips.length+' IP'] }
    }
    await Promise.all(cards.map(async card=>{try {const [value,detail]=await loaders[card.id](); if(!active.signal.aborted)metrics[card.id]={...card,value,detail}}catch(e){if(!active.signal.aborted)metrics[card.id]={...card,value:'Unavailable',detail:String(e)}}}))
    if(!active.signal.aborted) loading=false
  }
  onMount(()=>{void refresh();return ()=>controller?.abort()})
</script>
{#if cards.length}
<section class="panel"><div class="resource-heading"><h2>Module health</h2><button disabled={loading} onclick={refresh}>Refresh</button></div>
<div class="module-health-grid">{#each cards as card (card.id)}<button class="module-health" onclick={()=>onNavigate(card.target)}><span>{card.label}</span><strong>{metrics[card.id]?.value ?? 'Loading…'}</strong><small>{metrics[card.id]?.detail ?? ''}</small></button>{/each}</div></section>
{/if}
<style>
.module-health-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px}
.module-health{display:flex;flex-direction:column;align-items:flex-start;text-align:left;gap:8px;white-space:normal;padding:16px}
.module-health strong{font-size:1.4rem}
.module-health small{overflow-wrap:anywhere}
</style>
