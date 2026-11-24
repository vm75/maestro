export interface SharedModule {
  id: string
  availability: 'unsupported' | 'disabled' | 'unavailable' | 'ready'
  dependencies: string[]
}
export interface SharedDiscovery { platform: string; modules: SharedModule[] }
