"use client"

import { useState } from "react"
import {
  ChevronDown,
  ChevronRight,
  FolderOpen,
  FolderPlus,
  GitBranch,
  GitMerge,
  MoreHorizontal,
  Plus,
  Rocket,
  Settings,
  Trash2,
} from "lucide-react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import {
  openFolderWindow,
  openSettingsWindow,
  openProjectBootWindow,
} from "@/lib/api"
import { isDesktop, openFileDialog } from "@/lib/platform"
import type { FolderGroupDetail, FolderHistoryEntry } from "@/lib/types"
import { useFolderNav } from "@/contexts/folder-nav-context"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { CloneDialog } from "@/components/welcome/clone-dialog"
import { DirectoryBrowserDialog } from "@/components/shared/directory-browser-dialog"
import { resolveWelcomeError } from "@/components/welcome/error-utils"
import { cn } from "@/lib/utils"

export function FolderNavTree() {
  const t = useTranslations("WelcomePage")
  const {
    groups,
    loading,
    activeFolderId,
    expandedGroupIds,
    activateFolder,
    unloadFolder,
    toggleGroup,
    removeGroup,
  } = useFolderNav()
  const [cloneOpen, setCloneOpen] = useState(false)
  const [browserOpen, setBrowserOpen] = useState(false)

  const handleOpenFolder = async () => {
    if (isDesktop()) {
      const result = await openFileDialog({ directory: true, multiple: false })
      if (!result) return
      const selected = Array.isArray(result) ? result[0] : result
      try {
        await openFolderWindow(selected)
      } catch (err) {
        console.error("[FolderNavTree] failed to open folder:", err)
        const resolved = resolveWelcomeError(err)
        toast.error(t("toasts.openFolderFailed"), {
          description: resolved.detail ?? t(resolved.key),
        })
      }
    } else {
      setBrowserOpen(true)
    }
  }

  const handleBrowserSelect = async (path: string) => {
    try {
      await openFolderWindow(path)
    } catch (err) {
      console.error("[FolderNavTree] failed to open folder:", err)
      const resolved = resolveWelcomeError(err)
      toast.error(t("toasts.openFolderFailed"), {
        description: resolved.detail ?? t(resolved.key),
      })
    }
  }

  const handleProjectBoot = async () => {
    try {
      await openProjectBootWindow("main")
    } catch (err) {
      console.error("[FolderNavTree] failed to open project boot:", err)
      toast.error(t("toasts.openProjectBootFailed"))
    }
  }

  const handleOpenSettings = async () => {
    try {
      await openSettingsWindow()
    } catch (err) {
      console.error("[FolderNavTree] failed to open settings:", err)
      toast.error(t("toasts.openSettingsFailed"))
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col border-r border-border bg-sidebar text-sidebar-foreground">
      <div className="flex items-center gap-1 border-b border-border px-2 py-2">
        <span className="flex-1 px-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          Folders
        </span>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7"
              aria-label="Add folder"
            >
              <Plus className="h-4 w-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-56">
            <DropdownMenuItem onClick={handleOpenFolder}>
              <FolderOpen className="h-4 w-4" />
              <span>{t("openFolder")}</span>
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => setCloneOpen(true)}>
              <GitBranch className="h-4 w-4" />
              <span>{t("cloneRepository")}</span>
            </DropdownMenuItem>
            <DropdownMenuItem onClick={handleProjectBoot}>
              <Rocket className="h-4 w-4" />
              <span>{t("projectBoot")}</span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button
          variant="ghost"
          size="icon"
          className="h-7 w-7"
          aria-label="Open settings"
          onClick={handleOpenSettings}
        >
          <Settings className="h-4 w-4" />
        </Button>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto py-1">
        {loading && groups.length === 0 ? (
          <div className="px-3 py-6 text-center text-xs text-muted-foreground">
            {t("loading")}
          </div>
        ) : groups.length === 0 ? (
          <div className="px-3 py-6 text-center text-xs text-muted-foreground">
            {t("emptyFolders")}
          </div>
        ) : (
          groups.map((group) => (
            <FolderGroupRow
              key={group.id}
              group={group}
              expanded={expandedGroupIds.has(group.id)}
              activeFolderId={activeFolderId}
              onToggle={() => toggleGroup(group.id)}
              onRemoveGroup={async () => {
                try {
                  await removeGroup(group.id)
                } catch (err) {
                  console.error(err)
                  toast.error(String(err))
                }
              }}
              onSelectFolder={async (folder) => {
                if (folder.is_open) {
                  activateFolder(folder.id)
                  return
                }
                try {
                  // Calling openFolderWindow with the path flips is_open
                  // back to true and emits `activate-folder` which the
                  // context listener picks up to switch active state.
                  await openFolderWindow(folder.path)
                } catch (err) {
                  console.error("[FolderNavTree] load folder failed:", err)
                  const resolved = resolveWelcomeError(err)
                  toast.error(t("toasts.openFolderFailed"), {
                    description: resolved.detail ?? t(resolved.key),
                  })
                }
              }}
              onUnloadFolder={async (folderId) => {
                try {
                  await unloadFolder(folderId)
                } catch (err) {
                  console.error(err)
                  toast.error(String(err))
                }
              }}
            />
          ))
        )}
      </div>

      <CloneDialog open={cloneOpen} onOpenChange={setCloneOpen} />
      <DirectoryBrowserDialog
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        onSelect={handleBrowserSelect}
      />
    </div>
  )
}

interface FolderGroupRowProps {
  group: FolderGroupDetail
  expanded: boolean
  activeFolderId: number | null
  onToggle: () => void
  onRemoveGroup: () => void | Promise<void>
  onSelectFolder: (folder: FolderHistoryEntry) => void | Promise<void>
  onUnloadFolder: (folderId: number) => void | Promise<void>
}

function FolderGroupRow({
  group,
  expanded,
  activeFolderId,
  onToggle,
  onRemoveGroup,
  onSelectFolder,
  onUnloadFolder,
}: FolderGroupRowProps) {
  return (
    <div className="select-none">
      <div
        role="button"
        tabIndex={0}
        className="group flex items-center gap-1 px-2 py-1.5 text-sm hover:bg-sidebar-accent cursor-pointer"
        onClick={onToggle}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault()
            onToggle()
          }
        }}
        title={group.name}
      >
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" />
        )}
        <span className="flex-1 truncate font-medium">{group.name}</span>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="h-6 w-6 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
              onClick={(e) => e.stopPropagation()}
              aria-label="Group actions"
            >
              <MoreHorizontal className="h-3.5 w-3.5" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-48">
            <DropdownMenuItem
              onClick={(e) => {
                e.stopPropagation()
                void onRemoveGroup()
              }}
              variant="destructive"
            >
              <Trash2 className="h-4 w-4" />
              <span>Remove group</span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {expanded &&
        group.folders.map((folder) => (
          <FolderRow
            key={folder.id}
            folder={folder}
            isActive={folder.id === activeFolderId}
            onClick={() => onSelectFolder(folder)}
            onUnload={() => onUnloadFolder(folder.id)}
          />
        ))}
    </div>
  )
}

interface FolderRowProps {
  folder: FolderHistoryEntry
  isActive: boolean
  onClick: () => void
  onUnload: () => void | Promise<void>
}

function FolderRow({ folder, isActive, onClick, onUnload }: FolderRowProps) {
  const label = folder.git_branch ?? folder.name
  return (
    <div
      role="button"
      tabIndex={0}
      className={cn(
        "group flex items-center gap-1 pl-6 pr-2 py-1 text-sm cursor-pointer hover:bg-sidebar-accent",
        isActive && "bg-sidebar-primary text-sidebar-primary-foreground",
        !folder.is_open && !isActive && "text-muted-foreground"
      )}
      onClick={onClick}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault()
          onClick()
        }
      }}
      title={folder.name}
    >
      <GitMerge
        className={cn(
          "h-3.5 w-3.5 flex-shrink-0",
          folder.is_open ? "opacity-80" : "opacity-40"
        )}
      />
      <span className="flex-1 truncate">{label}</span>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            className={cn(
              "h-6 w-6 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100",
              isActive && "text-sidebar-primary-foreground"
            )}
            onClick={(e) => e.stopPropagation()}
            aria-label="Folder actions"
          >
            <MoreHorizontal className="h-3.5 w-3.5" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-48">
          {folder.is_open ? (
            <DropdownMenuItem
              onClick={(e) => {
                e.stopPropagation()
                void onUnload()
              }}
            >
              <FolderPlus className="h-4 w-4" />
              <span>Unload folder</span>
            </DropdownMenuItem>
          ) : (
            <DropdownMenuItem
              onClick={(e) => {
                e.stopPropagation()
                onClick()
              }}
            >
              <FolderOpen className="h-4 w-4" />
              <span>Load folder</span>
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled>
            <FolderOpen className="h-4 w-4" />
            <span>Remove from history</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
