import { CheckIcon, ChevronsUpDownIcon } from 'lucide-react'
import { useState } from 'react'
import { cn } from '../lib/utils'
import { Button } from './ui/button'
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from './ui/command'
import { Popover, PopoverContent, PopoverTrigger } from './ui/popover'

export type ComboboxOption = {
  value: string
  label: string
}

type ComboboxProps = {
  value: string
  onChange: (value: string) => void
  options: ComboboxOption[]
  placeholder: string
  searchPlaceholder: string
  emptyText: string
  ariaLabel: string
  allowCustom?: boolean
  customLabel?: (query: string) => string
  disabled?: boolean
}

function findSelectedOption(options: ComboboxOption[], value: string): ComboboxOption | undefined {
  return options.find((option) => option.value === value)
}

function hasExactMatch(options: ComboboxOption[], normalizedQuery: string): boolean {
  return options.some((option) => {
    return option.value.toLowerCase() === normalizedQuery || option.label.toLowerCase() === normalizedQuery
  })
}

function sortOptions(options: ComboboxOption[]): ComboboxOption[] {
  return [...options].sort((left, right) => left.label.localeCompare(right.label))
}

export function Combobox({
  value,
  onChange,
  options,
  placeholder,
  searchPlaceholder,
  emptyText,
  ariaLabel,
  allowCustom = false,
  customLabel,
  disabled = false,
}: ComboboxProps): React.JSX.Element {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')

  const normalizedQuery = query.trim().toLowerCase()
  const selectedOption = findSelectedOption(options, value)
  const displayValue = (selectedOption?.label ?? value) || placeholder
  const showCustomOption = allowCustom && normalizedQuery.length > 0 && !hasExactMatch(options, normalizedQuery)
  const sortedOptions = sortOptions(options)

  function selectValue(nextValue: string): void {
    onChange(nextValue)
    setOpen(false)
    setQuery('')
  }

  function handleOpenChange(nextOpen: boolean): void {
    setOpen(nextOpen)
    if (!nextOpen) {
      setQuery('')
    }
  }

  return (
    <Popover open={open} onOpenChange={handleOpenChange}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-label={ariaLabel}
          aria-expanded={open}
          className="w-full justify-between"
          disabled={disabled}
        >
          <span className="truncate">{displayValue}</span>
          <ChevronsUpDownIcon className="opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[var(--radix-popover-trigger-width)] p-0" align="start">
        <Command shouldFilter>
          <CommandInput placeholder={searchPlaceholder} value={query} onValueChange={setQuery} />
          <CommandList>
            <CommandEmpty>{emptyText}</CommandEmpty>
            <CommandGroup>
              {sortedOptions.map((option) => (
                <CommandItem key={option.value} value={option.value} onSelect={() => selectValue(option.value)}>
                  <CheckIcon className={cn('mr-2', value === option.value ? 'opacity-100' : 'opacity-0')} />
                  {option.label}
                </CommandItem>
              ))}
              {showCustomOption ? (
                <CommandItem value={`custom-${normalizedQuery}`} onSelect={() => selectValue(query.trim())}>
                  <CheckIcon className={cn('mr-2', value === query.trim() ? 'opacity-100' : 'opacity-0')} />
                  {customLabel ? customLabel(query.trim()) : query.trim()}
                </CommandItem>
              ) : null}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
