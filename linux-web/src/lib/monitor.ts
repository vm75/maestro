export function bytes(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—'
  if (value < 1024) return `${Math.round(value)} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB', 'PiB']
  let scaled = value / 1024, index = 0
  while (scaled >= 1024 && index < units.length - 1) { scaled /= 1024; index++ }
  return `${scaled.toFixed(scaled >= 100 ? 0 : 1)} ${units[index]}`
}
export function rate(value: number | null | undefined): string { return value == null ? '—' : `${bytes(value)}/s` }
export function percent(value: number | null | undefined): string { return value == null || !Number.isFinite(value) ? '—' : `${value.toFixed(1)}%` }
export function uptime(value: number | null | undefined): string {
  if (value == null) return '—'
  const days = Math.floor(value / 86400), hours = Math.floor(value % 86400 / 3600), minutes = Math.floor(value % 3600 / 60)
  return days ? `${days}d ${hours}h` : hours ? `${hours}h ${minutes}m` : `${minutes}m`
}
