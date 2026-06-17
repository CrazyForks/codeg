"use client"

import { Fragment } from "react"
import { ChevronDown } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { DropdownRadioItemContent } from "@/components/chat/dropdown-radio-item-content"
import type { SessionConfigOptionInfo } from "@/lib/types"

interface SessionConfigSelectorProps {
  option: SessionConfigOptionInfo
  onSelect: (configId: string, valueId: string) => void
}

export function SessionConfigSelector({
  option,
  onSelect,
}: SessionConfigSelectorProps) {
  if (option.kind.type !== "select") return null

  const allOptions =
    option.kind.groups.length > 0
      ? option.kind.groups.flatMap((group) => group.options)
      : option.kind.options
  const selected = allOptions.find(
    (item) => item.value === option.kind.current_value
  )
  const currentLabel = selected?.name ?? option.kind.current_value

  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger title={option.description ?? option.name}>
        <span className="min-w-0 flex-1 truncate font-medium">
          {option.name}
        </span>
        <span
          className="max-w-[10rem] shrink-0 truncate text-xs text-muted-foreground"
          title={currentLabel}
        >
          {currentLabel}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent
        className="min-w-72 max-w-xs overflow-y-auto"
        style={{
          maxHeight:
            "min(60vh, var(--radix-dropdown-menu-content-available-height))",
        }}
      >
        <DropdownMenuRadioGroup
          value={option.kind.current_value}
          onValueChange={(value) => onSelect(option.id, value)}
        >
          {option.kind.groups.length > 0
            ? option.kind.groups.map((group, index) => (
                <Fragment key={group.group}>
                  {index > 0 && <DropdownMenuSeparator />}
                  <DropdownMenuLabel>{group.name}</DropdownMenuLabel>
                  {group.options.map((item) => (
                    <DropdownMenuRadioItem
                      key={`${group.group}-${item.value}`}
                      value={item.value}
                      title={item.name}
                    >
                      <DropdownRadioItemContent
                        label={item.name}
                        description={item.description}
                      />
                    </DropdownMenuRadioItem>
                  ))}
                </Fragment>
              ))
            : option.kind.options.map((item) => (
                <DropdownMenuRadioItem
                  key={item.value}
                  value={item.value}
                  title={item.name}
                >
                  <DropdownRadioItemContent
                    label={item.name}
                    description={item.description}
                  />
                </DropdownMenuRadioItem>
              ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}

export function InlineSessionConfigSelector({
  option,
  onSelect,
}: SessionConfigSelectorProps) {
  if (option.kind.type !== "select") return null

  const allOptions =
    option.kind.groups.length > 0
      ? option.kind.groups.flatMap((group) => group.options)
      : option.kind.options
  const selected = allOptions.find(
    (item) => item.value === option.kind.current_value
  )
  const currentLabel = selected?.name ?? option.kind.current_value

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="xs"
          title={currentLabel}
          className="min-w-0 gap-0.5 px-1 text-muted-foreground"
        >
          <span className="max-w-[10rem] truncate">{currentLabel}</span>
          <ChevronDown className="size-3 shrink-0 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="start"
        className="min-w-72 overflow-y-auto"
        style={{
          maxWidth: "min(20rem, calc(100vw - 1rem))",
          maxHeight:
            "min(60vh, var(--radix-dropdown-menu-content-available-height))",
        }}
      >
        <DropdownMenuRadioGroup
          value={option.kind.current_value}
          onValueChange={(value) => onSelect(option.id, value)}
        >
          {option.kind.groups.length > 0
            ? option.kind.groups.map((group, index) => (
                <Fragment key={group.group}>
                  {index > 0 && <DropdownMenuSeparator />}
                  <DropdownMenuLabel>{group.name}</DropdownMenuLabel>
                  {group.options.map((item) => (
                    <DropdownMenuRadioItem
                      key={`${group.group}-${item.value}`}
                      value={item.value}
                      title={item.name}
                    >
                      <DropdownRadioItemContent
                        label={item.name}
                        description={item.description}
                      />
                    </DropdownMenuRadioItem>
                  ))}
                </Fragment>
              ))
            : option.kind.options.map((item) => (
                <DropdownMenuRadioItem
                  key={item.value}
                  value={item.value}
                  title={item.name}
                >
                  <DropdownRadioItemContent
                    label={item.name}
                    description={item.description}
                  />
                </DropdownMenuRadioItem>
              ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
