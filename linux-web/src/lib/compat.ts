// Keep the upstream Server Man editor's camelCase model. Translate only known
// schema fields; resource IDs, environment variables and arbitrary map keys
// must never be renamed.
const fields: Record<string, Record<string, string>> = {
  service: { depends_on: 'dependsOn' },
  container: {
    container_name: 'containerName', working_dir: 'workingDir', env_files: 'envFiles',
    memory_mib: 'memoryMiB', userns_mode: 'usernsMode', uid_maps: 'uidMaps',
    gid_maps: 'gidMaps', group_add: 'groupAdd', network_mode: 'networkMode',
    depends_on: 'dependsOn', cap_add: 'capAdd', cap_drop: 'capDrop', security_opt: 'securityOpt',
    read_only: 'readOnly', shm_size: 'shmSize', stop_grace_period: 'stopGracePeriod',
    pull_policy: 'pullPolicy', log_driver: 'logDriver', log_options: 'logOptions',
  },
  health: { start_period: 'startPeriod' },
  mount: { read_only: 'readOnly' },
  vm: { root_size_gib: 'rootSizeGiB', memory_mib: 'memoryMiB', instance_type: 'instanceType' },
  share: { read_only: 'readOnly', root_squash: 'rootSquash' },
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
function mapResources(value: unknown, kind: string, native: boolean): unknown {
  return isRecord(value) ? Object.fromEntries(Object.entries(value).map(([id, definition]) => [id, convertDefinition(kind, definition, native)])) : value
}
export function convertDefinition(kind: string, value: unknown, native: boolean): unknown {
  if (!isRecord(value)) return value
  const aliases = fields[kind] ?? {}
  const names = native ? Object.fromEntries(Object.entries(aliases).map(([wire, ui]) => [ui, wire])) : aliases
  return Object.fromEntries(Object.entries(value).map(([key, field]) => {
    let result = field
    if (kind === 'service' && key === 'containers') result = mapResources(field, 'container', native)
    if (kind === 'service' && key === 'vm') result = convertDefinition('vm', field, native)
    if ((kind === 'container' || kind === 'vm') && key === 'mounts' && Array.isArray(field)) result = field.map(m => convertDefinition('mount', m, native))
    if (kind === 'container' && key === 'healthcheck') result = convertDefinition('health', field, native)
    return [names[key] ?? key, result]
  }))
}
export function adaptToolResponse(name: string, value: unknown): unknown {
  if (!isRecord(value)) return value
  if (name === 'managed_config_model' && isRecord(value.config)) {
    const config = value.config
    return { ...value, config: { ...config, services: mapResources(config.services, 'service', false), shares: mapResources(config.shares, 'share', false) } }
  }
  if (name === 'managed_shares' && Array.isArray(value.shares)) {
    return { ...value, shares: value.shares.map(item => isRecord(item) ? { ...item, definition: convertDefinition('share', item.definition, false) } : item) }
  }
  return value
}
