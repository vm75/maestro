<script lang="ts">
  import { onMount, tick } from 'svelte'
  import { request } from '../lib/api'
  let id=$state<string|null>(null)
  let busy=$state(false)
  let error=$state('')
  let output=$state('Connect to open a local shell.')
  let input=$state('')
  let cursor=0
  let stopped=false
  let viewer:HTMLPreElement
  let decoder=new TextDecoder()
  let controller:AbortController|undefined
  function call<T>(action:Record<string,unknown>,signal?:AbortSignal){return request<T>('/api/terminal',{method:'POST',body:JSON.stringify(action),signal})}
  function release(target:string){void fetch('/api/terminal',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({action:'close',id:target}),keepalive:true}).catch(()=>{})}
  function disconnect(){const target=id;id=null;controller?.abort();if(target)release(target)}
  async function poll(target:string,signal:AbortSignal){
    try{
      while(!stopped&&!signal.aborted&&id===target){
        const result=await call<{data:string;cursor:number;truncated:boolean;closed:boolean}>({action:'poll',id:target,cursor},signal)
        if(stopped||signal.aborted||id!==target)return
        if(result.truncated){decoder=new TextDecoder();output+='\n[earlier output truncated]\n'}
        const bytes=Uint8Array.from(atob(result.data),c=>c.charCodeAt(0))
        output=(output+decoder.decode(bytes,{stream:!result.closed}).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g,'')).slice(-180000)
        cursor=result.cursor
        await tick();if(viewer)viewer.scrollTop=viewer.scrollHeight
        if(result.closed){id=null;return}
        await new Promise(resolve=>setTimeout(resolve,350))
      }
    }catch(failure){if(!signal.aborted&&!stopped){error=failure instanceof Error?failure.message:'Terminal connection lost';disconnect()}}
  }
  async function connect(){
    if(busy||id)return
    if(!window.confirm('Open an interactive shell as the Maestro workload account? Commands run on the local server.'))return
    busy=true;error=''
    try{
      const result=await call<{id:string}>({action:'open'})
      if(stopped){release(result.id);return}
      id=result.id;cursor=0;decoder=new TextDecoder();output=''
      controller=new AbortController();void poll(result.id,controller.signal)
    }catch(failure){error=failure instanceof Error?failure.message:'Could not open terminal'}
    finally{busy=false}
  }
  async function send(data:string){
    if(!id||busy)return
    busy=true;error=''
    try{await call({action:'input',id,data})}
    catch(failure){error=failure instanceof Error?failure.message:'Input failed'}
    finally{busy=false}
  }
  onMount(()=>{window.addEventListener('pagehide',disconnect);return()=>{stopped=true;disconnect();window.removeEventListener('pagehide',disconnect)}})
</script>
<section class="panel">
  <div class="toolbar"><h2>Browser terminal</h2>
    {#if id}<button onclick={disconnect}>Disconnect</button>{:else}<button class="primary" disabled={busy} onclick={connect}>Connect</button>{/if}
  </div>
  <p>Local non-root shell, one connection at a time. Disconnects after 45 seconds without browser contact or 30 minutes total. Supports commands and control keys, not full-screen terminal applications.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  <pre bind:this={viewer} class="transcript" aria-label="Terminal output" tabindex="0">{output}</pre>
  <form onsubmit={(e)=>{e.preventDefault();const line=input;input='';void send(line+'\n')}}>
    <label>Terminal input<input bind:value={input} disabled={!id||busy} maxlength="1000" autocomplete="off" spellcheck={false}/></label>
    <button type="submit" disabled={!id||busy}>Send</button>
    <button type="button" disabled={!id||busy} onclick={()=>send('\x03')}>Ctrl+C</button>
    <button type="button" disabled={!id||busy} onclick={()=>send('\x04')}>Ctrl+D</button>
  </form>
</section>
<style>
  .transcript{min-height:16rem;max-height:60vh;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere;background:#12171c;color:#e3e9ed;padding:1rem;border-radius:.5rem;}
  form{display:flex;align-items:end;gap:.5rem;flex-wrap:wrap;}form label{flex:1;min-width:12rem;}
</style>
