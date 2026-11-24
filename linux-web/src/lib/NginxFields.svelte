<script lang="ts">
  import { setField, settingValue, type Field, type Settings } from './nginx-config'
  let { fields, value = $bindable({}), inherited = {}, disabled = false }:
    {fields: Field[]; value: Settings; inherited?: Settings; disabled?: boolean} = $props()
  function update(field: Field, raw: string) { value = setField(value, field.key, settingValue(field, raw)) }
</script>
<div class="fields">
  {#each fields as field (field.key)}
    <div class="field">
      <label>{field.label}
        {#if field.type === 'bool' || field.type === 'select'}
          <select disabled={disabled} value={value[field.key] === undefined ? '' : String(value[field.key])} onchange={event => value = setField(value, field.key, event.currentTarget.value === '' ? undefined : settingValue(field,event.currentTarget.value))}>
            <option value="">Inherit ({String(inherited[field.key] ?? field.fallback ?? 'default')})</option>
            {#each field.type === 'bool' ? ['true','false'] : (field.options || []) as choice}
              <option value={choice}>{field.type === 'bool' ? choice === 'true' ? 'Yes' : 'No' : choice}</option>
            {/each}
          </select>
        {:else}
          <input type="text" inputmode={field.type === 'number' ? 'numeric' : 'text'} disabled={disabled || value[field.key] === undefined} value={String(value[field.key] ?? '')} placeholder={String(inherited[field.key] ?? field.fallback ?? '')} oninput={event => update(field,event.currentTarget.value)} />
        {/if}
      </label>
      {#if field.type !== 'bool' && field.type !== 'select'}
        <label class="override"><input type="checkbox" checked={value[field.key] !== undefined} disabled={disabled} onchange={event => value = setField(value,field.key,event.currentTarget.checked ? inherited[field.key] ?? field.fallback ?? '' : undefined)} /> Override inherited value</label>
      {/if}
      {#if field.help}<small>{field.help}</small>{/if}
    </div>
  {/each}
</div>
<style>
  .fields{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px}
  .field{min-width:0}
  .override{display:flex;align-items:center;gap:6px;font-size:.85em}
  .override input{width:auto}
</style>
