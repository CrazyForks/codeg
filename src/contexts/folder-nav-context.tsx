"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"
import { toErrorMessage } from "@/lib/app-error"
import {
  closeFolderWindow,
  listFolderGroups,
  removeFolderGroup,
  renameFolderGroup,
  reorderFolderGroups,
  reorderFoldersInGroup,
} from "@/lib/api"
import type { FolderGroupDetail, FolderHistoryEntry } from "@/lib/types"
import { getTransport } from "@/lib/transport"

const ACTIVE_FOLDER_STORAGE_KEY = "codeg:nav:active_folder_id"
const EXPANDED_GROUPS_STORAGE_KEY = "codeg:nav:expanded_group_ids"

function readStorageNumber(key: string): number | null {
  if (typeof window === "undefined") return null
  const raw = window.localStorage.getItem(key)
  if (raw == null) return null
  const parsed = Number.parseInt(raw, 10)
  return Number.isFinite(parsed) ? parsed : null
}

function writeStorageNumber(key: string, value: number | null) {
  if (typeof window === "undefined") return
  if (value == null) {
    window.localStorage.removeItem(key)
  } else {
    window.localStorage.setItem(key, String(value))
  }
}

function readStorageSet(key: string): Set<number> {
  if (typeof window === "undefined") return new Set()
  try {
    const raw = window.localStorage.getItem(key)
    if (!raw) return new Set()
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return new Set()
    return new Set(
      parsed.filter(
        (v): v is number => typeof v === "number" && Number.isFinite(v)
      )
    )
  } catch {
    return new Set()
  }
}

function writeStorageSet(key: string, value: Set<number>) {
  if (typeof window === "undefined") return
  window.localStorage.setItem(key, JSON.stringify(Array.from(value)))
}

interface FolderNavContextValue {
  groups: FolderGroupDetail[]
  loading: boolean
  error: string | null
  activeFolderId: number | null
  expandedGroupIds: Set<number>
  /** Folders currently held in-memory (is_open=true). Ordered by group then sort. */
  loadedFolders: FolderHistoryEntry[]

  refresh: () => Promise<void>
  activateFolder: (folderId: number) => void
  clearActiveFolder: () => void
  unloadFolder: (folderId: number) => Promise<void>
  toggleGroup: (groupId: number) => void
  expandGroup: (groupId: number) => void
  renameGroup: (groupId: number, name: string) => Promise<void>
  removeGroup: (groupId: number) => Promise<void>
  reorderGroups: (orderedIds: number[]) => Promise<void>
  reorderFolders: (groupId: number, orderedFolderIds: number[]) => Promise<void>

  findFolder: (folderId: number) => FolderHistoryEntry | null
}

const FolderNavContext = createContext<FolderNavContextValue | null>(null)

export function useFolderNav(): FolderNavContextValue {
  const ctx = useContext(FolderNavContext)
  if (!ctx) {
    throw new Error("useFolderNav must be used within FolderNavProvider")
  }
  return ctx
}

export function FolderNavProvider({ children }: { children: ReactNode }) {
  const [groups, setGroups] = useState<FolderGroupDetail[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [activeFolderId, setActiveFolderIdState] = useState<number | null>(null)
  const [expandedGroupIds, setExpandedGroupIds] = useState<Set<number>>(() =>
    readStorageSet(EXPANDED_GROUPS_STORAGE_KEY)
  )
  const initialRestoreRef = useRef(false)

  const persistActiveFolder = useCallback((id: number | null) => {
    writeStorageNumber(ACTIVE_FOLDER_STORAGE_KEY, id)
  }, [])

  const persistExpanded = useCallback((set: Set<number>) => {
    writeStorageSet(EXPANDED_GROUPS_STORAGE_KEY, set)
  }, [])

  const refresh = useCallback(async () => {
    try {
      const result = await listFolderGroups()
      setGroups(result)
      setError(null)
    } catch (err) {
      setError(toErrorMessage(err))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])

  // First-load active folder restoration:
  // 1. Use localStorage value if it still maps to a loaded (is_open=true) folder
  // 2. Otherwise fall back to the first loaded folder (if any)
  // 3. Otherwise stay in empty state
  useEffect(() => {
    if (initialRestoreRef.current || loading) return
    initialRestoreRef.current = true
    const stored = readStorageNumber(ACTIVE_FOLDER_STORAGE_KEY)
    const isLoaded = (id: number) =>
      groups.some((g) => g.folders.some((f) => f.id === id && f.is_open))

    if (stored != null && isLoaded(stored)) {
      setActiveFolderIdState(stored)
      const group = groups.find((g) => g.folders.some((f) => f.id === stored))
      if (group) {
        setExpandedGroupIds((prev) => {
          if (prev.has(group.id)) return prev
          const next = new Set(prev)
          next.add(group.id)
          persistExpanded(next)
          return next
        })
      }
      return
    }

    const firstLoaded = groups
      .flatMap((g) => g.folders.map((f) => ({ folder: f, group: g })))
      .find((x) => x.folder.is_open)

    if (firstLoaded) {
      setActiveFolderIdState(firstLoaded.folder.id)
      persistActiveFolder(firstLoaded.folder.id)
      setExpandedGroupIds((prev) => {
        if (prev.has(firstLoaded.group.id)) return prev
        const next = new Set(prev)
        next.add(firstLoaded.group.id)
        persistExpanded(next)
        return next
      })
    } else {
      persistActiveFolder(null)
    }
  }, [groups, loading, persistActiveFolder, persistExpanded])

  // Refresh whenever the backend broadcasts folder-group / folder changes.
  useEffect(() => {
    const transport = getTransport()
    const unsubs: Array<() => void> = []
    let cancelled = false

    for (const event of ["folder-group-updated", "folder-updated"]) {
      void transport
        .subscribe(event, () => {
          void refresh()
        })
        .then((unsub) => {
          if (cancelled) {
            unsub()
          } else {
            unsubs.push(unsub)
          }
        })
    }

    void transport
      .subscribe<FolderHistoryEntry>("activate-folder", (entry) => {
        if (!entry || typeof entry.id !== "number") return
        setActiveFolderIdState(entry.id)
        persistActiveFolder(entry.id)
        if (entry.group_id != null) {
          setExpandedGroupIds((prev) => {
            if (prev.has(entry.group_id)) return prev
            const next = new Set(prev)
            next.add(entry.group_id)
            persistExpanded(next)
            return next
          })
        }
        void refresh()
      })
      .then((unsub) => {
        if (cancelled) {
          unsub()
        } else {
          unsubs.push(unsub)
        }
      })

    return () => {
      cancelled = true
      for (const u of unsubs) u()
    }
  }, [persistActiveFolder, persistExpanded, refresh])

  const activateFolder = useCallback(
    (folderId: number) => {
      setActiveFolderIdState(folderId)
      persistActiveFolder(folderId)
      const group = groups.find((g) => g.folders.some((f) => f.id === folderId))
      if (group) {
        setExpandedGroupIds((prev) => {
          if (prev.has(group.id)) return prev
          const next = new Set(prev)
          next.add(group.id)
          persistExpanded(next)
          return next
        })
      }
    },
    [groups, persistActiveFolder, persistExpanded]
  )

  const clearActiveFolder = useCallback(() => {
    setActiveFolderIdState(null)
    persistActiveFolder(null)
  }, [persistActiveFolder])

  const unloadFolder = useCallback(
    async (folderId: number) => {
      await closeFolderWindow(folderId)
      if (activeFolderId === folderId) {
        clearActiveFolder()
      }
      await refresh()
    },
    [activeFolderId, clearActiveFolder, refresh]
  )

  const toggleGroup = useCallback(
    (groupId: number) => {
      setExpandedGroupIds((prev) => {
        const next = new Set(prev)
        if (next.has(groupId)) {
          next.delete(groupId)
        } else {
          next.add(groupId)
        }
        persistExpanded(next)
        return next
      })
    },
    [persistExpanded]
  )

  const expandGroup = useCallback(
    (groupId: number) => {
      setExpandedGroupIds((prev) => {
        if (prev.has(groupId)) return prev
        const next = new Set(prev)
        next.add(groupId)
        persistExpanded(next)
        return next
      })
    },
    [persistExpanded]
  )

  const renameGroup = useCallback(
    async (groupId: number, name: string) => {
      await renameFolderGroup(groupId, name)
      await refresh()
    },
    [refresh]
  )

  const removeGroup = useCallback(
    async (groupId: number) => {
      const removedFolder = groups
        .find((g) => g.id === groupId)
        ?.folders.find((f) => f.id === activeFolderId)
      await removeFolderGroup(groupId)
      if (removedFolder) {
        clearActiveFolder()
      }
      await refresh()
    },
    [activeFolderId, clearActiveFolder, groups, refresh]
  )

  const reorderGroups = useCallback(
    async (orderedIds: number[]) => {
      await reorderFolderGroups(orderedIds)
      await refresh()
    },
    [refresh]
  )

  const reorderFolders = useCallback(
    async (groupId: number, orderedFolderIds: number[]) => {
      await reorderFoldersInGroup(groupId, orderedFolderIds)
      await refresh()
    },
    [refresh]
  )

  const findFolder = useCallback(
    (folderId: number): FolderHistoryEntry | null => {
      for (const g of groups) {
        const f = g.folders.find((f) => f.id === folderId)
        if (f) return f
      }
      return null
    },
    [groups]
  )

  const loadedFolders = useMemo<FolderHistoryEntry[]>(() => {
    const out: FolderHistoryEntry[] = []
    for (const g of groups) {
      for (const f of g.folders) {
        if (f.is_open) out.push(f)
      }
    }
    return out
  }, [groups])

  const value = useMemo<FolderNavContextValue>(
    () => ({
      groups,
      loading,
      error,
      activeFolderId,
      expandedGroupIds,
      loadedFolders,
      refresh,
      activateFolder,
      clearActiveFolder,
      unloadFolder,
      toggleGroup,
      expandGroup,
      renameGroup,
      removeGroup,
      reorderGroups,
      reorderFolders,
      findFolder,
    }),
    [
      groups,
      loading,
      error,
      activeFolderId,
      expandedGroupIds,
      loadedFolders,
      refresh,
      activateFolder,
      clearActiveFolder,
      unloadFolder,
      toggleGroup,
      expandGroup,
      renameGroup,
      removeGroup,
      reorderGroups,
      reorderFolders,
      findFolder,
    ]
  )

  return (
    <FolderNavContext.Provider value={value}>
      {children}
    </FolderNavContext.Provider>
  )
}
