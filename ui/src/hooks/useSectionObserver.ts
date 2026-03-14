import { useEffect, useRef, useState } from 'react'

/**
 * Observes a set of section elements by ID and returns the ID of the
 * section currently most visible in the viewport. Used for sticky
 * tab navigation that highlights the active section on scroll.
 */
export function useSectionObserver(sectionIds: string[]): string | null {
  const [activeId, setActiveId] = useState<string | null>(null)
  const ratioMap = useRef<Map<string, number>>(new Map())

  useEffect(() => {
    if (sectionIds.length === 0) return

    ratioMap.current.clear()

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          ratioMap.current.set(entry.target.id, entry.intersectionRatio)
        }

        // Pick the section with the highest visibility ratio
        let bestId: string | null = null
        let bestRatio = 0
        for (const [id, ratio] of ratioMap.current) {
          if (ratio > bestRatio) {
            bestRatio = ratio
            bestId = id
          }
        }

        if (bestId) {
          setActiveId(bestId)
        }
      },
      {
        // Multiple thresholds for granular ratio reporting
        threshold: [0, 0.1, 0.25, 0.5, 0.75, 1],
        // Offset by root header (48px) + section nav (~40px)
        rootMargin: '-88px 0px 0px 0px',
      },
    )

    const elements: Element[] = []
    for (const id of sectionIds) {
      const el = document.getElementById(id)
      if (el) {
        observer.observe(el)
        elements.push(el)
      }
    }

    return () => {
      for (const el of elements) {
        observer.unobserve(el)
      }
      observer.disconnect()
    }
  }, [sectionIds])

  return activeId
}
