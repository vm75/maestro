// The configuration model is shared with the router renderer. An absent key
// means inherit; false, zero, an empty string and an empty route list are explicit.
export type Settings = Record<string, unknown>
export type Endpoint = { address: string; port: number }
export type Listener = { https: Endpoint[]; http: Endpoint[]; http_redirect: boolean }
export type Access = { allow: string[] }
export type Limit = { rate: string; burst: number; nodelay: boolean; connections: number }
export type HttpAccess = { mode: 'off' | 'renewal_only' | 'always'; window_seconds: number; listener_profile: string; wan: {interface: string; target: string} | null; externally_managed_wan: boolean }
export type GlobalSettings = {
  shared: Settings; site_defaults: Settings; listener_profiles: Record<string, Listener>;
  access_policies: Record<string, Access>; limit_policies: Record<string, Limit>;
  allowed_ips: string[]; free_subdomains: string[]; http_access?: HttpAccess;
}
export type EffectiveAccess = {
  listener_profile: string; access_policy: string; https: Endpoint[]; http: Endpoint[];
  allowed_sources: string[] | null; inherited_from: string | null;
  routes: {path: string; access_policy: string; allowed_sources: string[] | null}[];
}
export type Usage = Record<string, string[]>
export type Field = {key: string; label: string; type: 'text' | 'bool' | 'number' | 'worker' | 'select'; fallback?: string | number | boolean; options?: string[]; help?: string; domainOnly?: boolean}
const settingHelp: Record<string,string> = {
      limit_policy: 'Named request and concurrent-request budget from Global settings. Blank inherits; default restores Ban Shield 30r/s, burst 60, immediate, 30 concurrent. Different names have independent per-IP budgets; reusing a name shares its budget.',
      enable_lanonly: 'Allow only clients listed in Global settings → Allowed client IPs / CIDRs. A parent domain’s LAN restriction also applies to its subfolders.',
      enable_subfolders: 'Domain only: load enabled child apps at paths such as /music/. Subdomains are configured separately.',
      disable_root: 'Domain only: return 404 for requests outside enabled subfolders and ACME routes. Set No to expose the root app’s upstream and proxy settings.',
      enable_large_upload: 'Send uploaded request bodies to the backend without buffering them in nginx temporary files. Uses Large upload limit and the extended session/upload idle timeouts.',
      enable_longlived: 'Extend idle read, write, and client-send timeouts for long jobs, streams, or idle WebSockets. Session timeout controls the idle gap, not total connection lifetime.',
      enable_streaming: 'Deliver backend responses as they arrive instead of buffering them. Useful for streaming media and live updates; SSE always streams.',
      enable_byte_ranges: 'Enable partial responses and forward Range / If-Range headers for media seeking and resumable downloads.',
      enable_restricted_sessions: 'Add a LAN-only /Sessions route under this app and suppress its ordinary access logs. Useful for media-server session APIs.',
      clear_origin: 'Remove the Origin header sent to the backend. Leave No unless the application specifically requires this.',
      access_log: 'Write normal request logs. Setting No keeps ordinary requests quiet while security traps remain logged.',
      backend_auth: 'Pass Authorization and WWW-Authenticate through to the backend and disable nginx basic authentication for this app. The backend authenticates users.',
      hide_etag: 'Remove the backend’s ETag response header. Useful for app-shell routes that should not reuse cached versions.',
      disable_proxy_redirect: 'Pass backend redirects through without nginx rewriting them. Useful when the application already knows its public URL.',
      client_max_body_size: 'Maximum request/upload body in normal proxy mode, such as 128M. Does not limit downloads. Large uploads uses its separate limit; 0 means unlimited.',
      large_upload_max_body_size: 'Maximum request/upload body while Large uploads is enabled, such as 1G. Replaces Body size limit in that mode; 0 means unlimited.',
      proxy_connect_timeout: 'Maximum time to establish a connection to the backend, such as 10s. This is separate from the idle timeout of an established connection.',
      proxy_session_timeout: 'Idle time allowed between upstream reads/writes and client sends for long-lived connections or large uploads. Not a total session deadline.',
      client_upload_timeout: 'Allowed idle gap between reads of an uploaded request body when Large uploads is enabled. Not a total upload deadline.',
      proxy_transport: 'websocket supports both normal HTTP and upgrades; http omits upgrade headers. sse clears Connection, always streams, and uses 300s connection/session timeouts unless a route overrides them.',
      cache_policy: 'inherit keeps the app’s cache headers; no_cache prevents storage; pwa keeps the shell fresh and caches static assets; immutable long-caches versioned files.',
      forwarded_for: 'replace uses nginx’s observed client IP. append adds it to the incoming X-Forwarded-For chain and is intended for trusted proxy setups.',
      spa_fallback: 'On an upstream 404, serve this URI, commonly /index.html, for client-side routing. Exclude API/auth routes. Leave blank for apps that should return their own 404s.',
      rewrite_to: 'Rewrite matching requests to this one backend URI before proxying. Leave blank for normal path handling, including automatic subfolder-prefix removal.',
      cors_origin: 'Add CORS response headers allowing GET / OPTIONS from this origin, or *. The backend still needs to handle any required preflight requests.',
      router_ip: 'Local address nginx listens on for the generated HTTP and HTTPS servers. This address must exist on the nginx host; it is not the backend address.',
      http_port: 'Listening port for HTTP redirects and HTTP-01 certificate challenges, normally 80.',
      https_port: 'Listening port for TLS sites, normally 443.',
      worker_processes: 'Number of nginx worker processes, or auto. Choose a value appropriate for the host’s CPU and memory.',
      worker_connections: 'Maximum simultaneous connections per worker. Proxied traffic uses connections to both clients and backends.',
      resolver_addr: 'DNS server nginx uses to resolve backend hostnames. It must be reachable from the nginx host.',
      resolver_validity: 'How long nginx caches DNS answers before refreshing them, such as 30s.',
      var_dir: 'Base directory for generated access and security logs. Maestro’s main configuration separately controls nginx’s PID and error-log paths.',
      acme_challenge_dir: 'Directory containing HTTP-01 challenge files. It must match the certificate issuer’s challenge directory and be readable by nginx.',
      default_certificate_dir: 'Certificate directory for the unknown-host TLS server. Requires fullchain.pem and privkey.pem. Relative paths resolve beneath the nginx root.',
      nginx_daemon: 'Defaults to Yes on Asuswrt-Merlin, Linux, and in test mode. No keeps nginx in the foreground and prevents init.d startup scripts from continuing. Disable only for a foreground supervisor.',
      nginx_user: 'When Maestro runs as root, workers use this account or nobody if blank. When Maestro runs as non-root, the generated configuration omits the user directive.',
      temp_dir: 'Writable parent for nginx’s client-body, proxy, fastcgi, uwsgi, and scgi temporary folders. Only the parent folder is configurable.',
      pwa_shell_cache_control: 'Cache-Control header for PWA app-shell files. The default prevents stale shell files from being stored.',
      pwa_asset_cache_control: 'Cache-Control header for static PWA assets. Long-lived caching requires versioned asset URLs so application updates are picked up.',
      allowed_ips: 'One client IP or CIDR per line. LAN-only sites deny clients outside this list; an empty list denies all clients to those locations.',
      free_subdomains: 'One shared domain suffix per line, such as duckdns.org. Used to select certificate folders for individual accounts; put more specific suffixes first.',
      domain: 'Domain group and hostname for this root site, such as example.com. Add aliases separately if the site answers to multiple hostnames.',
      parent_domain: 'Existing domain group that owns this child site. The parent must be enabled before its child sites can be served.',
      site_name: 'Name of the subdomain or subfolder within its parent. A subfolder name becomes the public path prefix, such as /music/.',
      server_names: 'Hostnames and aliases accepted by this server, separated by spaces or new lines. Leave blank to use the name derived from the site identity.',
      upstream: 'Internal HTTP or HTTPS URL of the application, including its port if needed. Subfolder routes remove their public prefix before forwarding.',
      enabled: 'Enable this site after creating it. Nginx configuration is tested before applying the change; a parent domain must be enabled to serve its children.',
      content: 'Authored nginx text for an existing custom site. It is not processed as Jinja or regenerated when managed defaults change.',
      routes: 'JSON array of route overrides. Paths are relative to the app mount. Blank inherits global routes; [] disables them. Feature settings can also supply defaults to child routes.',
      default_routes: 'Default JSON route policies inherited by managed sites. Each site can replace this list. Route paths are relative to that app’s mount.'
    }
export const policyFields: Field[] = [
  {key:'limit_policy',label:'Request limit policy',type:'select',fallback:'default'},
  {key:'enable_subfolders',label:'Enable subfolders',type:'bool',fallback:true,domainOnly:true},
  {key:'disable_root',label:'Return 404 at domain root',type:'bool',fallback:true,domainOnly:true},
  {key:'enable_large_upload',label:'Large uploads',type:'bool',fallback:false},
  {key:'enable_longlived',label:'Long-lived connections',type:'bool',fallback:false},
  {key:'enable_streaming',label:'Stream responses',type:'bool',fallback:false},
  {key:'enable_byte_ranges',label:'Byte ranges',type:'bool',fallback:false},
  {key:'enable_restricted_sessions',label:'Restricted Sessions route',type:'bool',fallback:false},
  {key:'clear_origin',label:'Clear Origin header',type:'bool',fallback:false},
  {key:'access_log',label:'Ordinary access logging',type:'bool',fallback:true},
  {key:'backend_auth',label:'Backend authentication',type:'bool',fallback:false},
  {key:'hide_etag',label:'Hide ETag',type:'bool',fallback:false},
  {key:'disable_proxy_redirect',label:'Disable proxy redirects',type:'bool',fallback:false},
  {key:'client_max_body_size',label:'Body size limit',type:'text',fallback:'100M'},
  {key:'large_upload_max_body_size',label:'Large upload limit',type:'text',fallback:'5G'},
  {key:'proxy_connect_timeout',label:'Connect timeout',type:'text',fallback:'10s'},
  {key:'proxy_session_timeout',label:'Session timeout',type:'text',fallback:'3600s'},
  {key:'client_upload_timeout',label:'Upload idle timeout',type:'text',fallback:'3600s'},
  {key:'proxy_transport',label:'Transport',type:'select',options:['websocket','http','sse'],fallback:'websocket'},
  {key:'cache_policy',label:'Cache policy',type:'select',options:['inherit','no_cache','immutable','pwa'],fallback:'inherit'},
  {key:'forwarded_for',label:'Forwarded client chain',type:'select',options:['replace','append'],fallback:'replace'},
  {key:'spa_fallback',label:'SPA fallback URI',type:'text',fallback:''},
  {key:'rewrite_to',label:'Rewrite URI',type:'text',fallback:''},
  {key:'cors_origin',label:'CORS origin',type:'text',fallback:''}
].map(field => ({...field,help:settingHelp[field.key]})) as Field[]
export const sharedFields: Field[] = [
  {key:'router_ip',label:'Legacy listen address',type:'text',fallback:'127.0.0.1'},
  {key:'http_port',label:'Legacy HTTP port',type:'number',fallback:80},
  {key:'https_port',label:'Legacy HTTPS port',type:'number',fallback:443},
  {key:'worker_processes',label:'Worker processes (or auto)',type:'worker',fallback:2},
  {key:'worker_connections',label:'Worker connections',type:'number',fallback:64},
  {key:'resolver_addr',label:'DNS resolver',type:'text',fallback:'127.0.0.1'},
  {key:'resolver_validity',label:'DNS validity',type:'text',fallback:'30s'},
  {key:'var_dir',label:'Runtime directory',type:'text'},
  {key:'acme_challenge_dir',label:'ACME challenge directory',type:'text'},
  {key:'default_certificate_dir',label:'Fallback certificate directory',type:'text'},
  {key:'nginx_daemon',label:'Run nginx in background',type:'bool',fallback:true},
  {key:'nginx_user',label:'Worker user (root only)',type:'text',fallback:'nobody',help:'Ignored when Maestro runs as non-root.'},
  {key:'temp_dir',label:'Temporary files parent directory',type:'text'},
  {key:'pwa_shell_cache_control',label:'PWA shell Cache-Control',type:'text',fallback:'no-cache, no-store, must-revalidate'},
  {key:'pwa_asset_cache_control',label:'PWA asset Cache-Control',type:'text',fallback:'public, max-age=31536000, immutable'}
].map(field => ({...field,help:settingHelp[field.key]})) as Field[]
export function proxyActive(kind: string, value: Settings, inherited: Settings): boolean { return kind !== 'domain' || !(value.disable_root ?? inherited.disable_root ?? true) }
export function visiblePolicyFields(kind: string, value: Settings, inherited: Settings): Field[] {
  const effective = (key:string) => value[key] ?? inherited[key] ?? policyFields.find(f=>f.key===key)?.fallback
  const proxy=proxyActive(kind,value,inherited), large=effective('enable_large_upload'), sse=effective('proxy_transport')==='sse'
  const routes=value.routes ?? inherited.routes, routeDefaults=Array.isArray(routes)&&routes.length>0
  return policyFields.filter(f=>{
    if(f.domainOnly)return kind==='domain'
    if(!proxy)return false
    if(['large_upload_max_body_size','client_upload_timeout'].includes(f.key))return !!large||routeDefaults
    if(f.key==='client_max_body_size')return !large||routeDefaults
    if(f.key==='proxy_session_timeout')return !sse&&!!(effective('enable_longlived')||large||routeDefaults)
    if(f.key==='proxy_connect_timeout')return !sse
    if(f.key==='enable_longlived')return (!sse&&!large)||routeDefaults
    if(f.key==='enable_streaming')return !sse||routeDefaults
    return true
  })
}
export const lines = (value: string) => value.split(/\r?\n/).map(s => s.trim()).filter(Boolean)
export const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value))
export function normalizeGlobal(value: GlobalSettings): GlobalSettings {
  return {...clone(value), listener_profiles: value.listener_profiles || {}, access_policies: value.access_policies || {},
    limit_policies: value.limit_policies || {}, allowed_ips: value.allowed_ips || [], free_subdomains: value.free_subdomains || [],
    shared: value.shared || {}, site_defaults: value.site_defaults || {}}
}
export const accessNames = (global: GlobalSettings) => ['legacy','public','deny',...Object.keys(global.access_policies)]
export const listenerNames = (global: GlobalSettings) => ['legacy',...Object.keys(global.listener_profiles)]
export function setField(settings: Settings, key: string, value: unknown): Settings {
  const next = {...settings}
  if (value === undefined) delete next[key]
  else next[key] = value
  return next
}
export function accessValue(settings: Settings): string {
  return String(settings.access_policy ?? (settings.enable_lanonly === undefined ? '' : settings.enable_lanonly ? 'legacy' : 'public'))
}
export function setAccess(settings: Settings, value: string): Settings {
  const next = setField(settings,'enable_lanonly',undefined)
  return setField(next,'access_policy',value || undefined)
}
export function settingValue(field: Field, raw: string): unknown {
  if (field.type === 'bool') return raw === 'true'
  if (field.type === 'number' || (field.type === 'worker' && raw !== 'auto')) return raw.trim() ? Number(raw) : undefined
  return raw
}
