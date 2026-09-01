"use client";

import { useSearchParams, useRouter, usePathname } from "next/navigation";
import { useState, useEffect, useCallback, useRef, useMemo } from "react";

export interface FreelancerFilters {
  search: string;
  skills: string[];
  page: number;
}

const DEFAULTS: FreelancerFilters = {
  search: "",
  skills: [],
  page: 1,
};

function parseFiltersFromParams(searchParams: URLSearchParams): FreelancerFilters {
  const skills = searchParams.get("skill");
  const page = parseInt(searchParams.get("page") || "1", 10);

  return {
    search: searchParams.get("search") || "",
    skills: skills ? skills.split(",") : [],
    page: isNaN(page) ? 1 : page,
  };
}

function filtersToParams(filters: FreelancerFilters): URLSearchParams {
  const params = new URLSearchParams();
  if (filters.search) params.set("search", filters.search);
  if (filters.skills.length) params.set("skill", filters.skills.join(","));
  if (filters.page > 1) params.set("page", String(filters.page));
  return params;
}

export function useFreelancerFilters() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const debounceRef = useRef<NodeJS.Timeout>();

  const filtersFromUrl = useMemo(
    () => parseFiltersFromParams(searchParams),
    [searchParams],
  );

  const [filters, setFilters] = useState<FreelancerFilters>(filtersFromUrl);
  const [debouncedSearch, setDebouncedSearch] = useState(filtersFromUrl.search);

  useEffect(() => {
    setFilters(filtersFromUrl);
    setDebouncedSearch(filtersFromUrl.search);
  }, [filtersFromUrl]);

  useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, []);

  const syncToUrl = useCallback(
    (next: FreelancerFilters) => {
      const qs = filtersToParams(next).toString();
      router.push(qs ? `${pathname}?${qs}` : pathname, { scroll: false });
    },
    [router, pathname],
  );

  const updateFilter = useCallback(
    <K extends keyof FreelancerFilters>(key: K, value: FreelancerFilters[K]) => {
      setFilters((prev) => {
        const next = {
          ...prev,
          [key]: value,
          page: key === "page" ? (value as number) : 1,
        };
        syncToUrl(next);
        return next;
      });
    },
    [syncToUrl],
  );

  const updateSearch = useCallback(
    (value: string) => {
      setFilters((prev) => ({ ...prev, search: value }));
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        setDebouncedSearch(value);
        setFilters((prev) => {
          const next = { ...prev, search: value, page: 1 };
          syncToUrl(next);
          return next;
        });
      }, 500);
    },
    [syncToUrl],
  );

  const toggleSkill = useCallback(
    (skill: string) => {
      setFilters((prev) => {
        const skills = prev.skills.includes(skill)
          ? prev.skills.filter((s) => s !== skill)
          : [...prev.skills, skill];
        const next = { ...prev, skills, page: 1 };
        syncToUrl(next);
        return next;
      });
    },
    [syncToUrl],
  );

  const clearAll = useCallback(() => {
    setFilters(DEFAULTS);
    setDebouncedSearch("");
    router.push(pathname, { scroll: false });
  }, [router, pathname]);

  const activeCount = useMemo(() => {
    let count = 0;
    if (filters.search) count++;
    if (filters.skills.length) count++;
    return count;
  }, [filters]);

  return {
    filters,
    debouncedSearch,
    updateFilter,
    updateSearch,
    toggleSkill,
    clearAll,
    activeCount,
  };
}
