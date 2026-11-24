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
]
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
]
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
