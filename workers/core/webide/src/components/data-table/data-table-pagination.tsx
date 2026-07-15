import type { Table } from "@tanstack/react-table";
import * as React from "react";

import { Button } from "@edger/ui/components/ui/button";
import { Combobox } from "@edger/ui/components/ui/combobox";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  ChevronsLeftIcon,
  ChevronsRightIcon,
} from "@edger/ui/icons/lucide";
import { cn } from "@edger/ui/lib/utils";

interface DataTablePaginationProps<TData> extends React.ComponentProps<"div"> {
  table: Table<TData>;
  pageSizeOptions?: number[];
}

export function DataTablePagination<TData>({
  table,
  pageSizeOptions = [10, 25, 50],
  className,
  ...props
}: DataTablePaginationProps<TData>) {
  const pageCount = Math.max(table.getPageCount(), 1);
  const page = table.getState().pagination.pageIndex + 1;
  const pageSize = table.getState().pagination.pageSize;
  const sizeOptions = React.useMemo(
    () =>
      pageSizeOptions.map((size) => ({
        value: `${size}`,
        label: `${size}`,
      })),
    [pageSizeOptions],
  );

  return (
    <div
      className={cn(
        "flex w-full flex-wrap items-center justify-end gap-3 overflow-auto p-1 sm:gap-4",
        className,
      )}
      {...props}
    >
      <div className="flex items-center justify-center font-medium text-sm">
        Page {page} of {pageCount}
      </div>
      <div className="flex items-center gap-2">
        <Button
          aria-label="Go to first page"
          className="hidden lg:inline-flex"
          disabled={!table.getCanPreviousPage()}
          onClick={() => table.setPageIndex(0)}
          size="icon"
          variant="outline"
        >
          <ChevronsLeftIcon />
        </Button>
        <Button
          aria-label="Go to previous page"
          disabled={!table.getCanPreviousPage()}
          onClick={() => table.previousPage()}
          size="icon"
          variant="outline"
        >
          <ChevronLeftIcon />
        </Button>
        <Button
          aria-label="Go to next page"
          disabled={!table.getCanNextPage()}
          onClick={() => table.nextPage()}
          size="icon"
          variant="outline"
        >
          <ChevronRightIcon />
        </Button>
        <Button
          aria-label="Go to last page"
          className="hidden lg:inline-flex"
          disabled={!table.getCanNextPage()}
          onClick={() => table.setPageIndex(table.getPageCount() - 1)}
          size="icon"
          variant="outline"
        >
          <ChevronsRightIcon />
        </Button>
      </div>
      <Combobox
        aria-label="Rows per page"
        onValueChange={(value) => table.setPageSize(Number(value))}
        options={sizeOptions}
        searchable={false}
        side="top"
        triggerClassName="h-8 w-18"
        value={`${pageSize}`}
      />
    </div>
  );
}
