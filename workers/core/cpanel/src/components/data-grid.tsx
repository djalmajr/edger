import {
  type Column,
  flexRender,
  type Table as TanstackTable,
} from "@tanstack/react-table";

import { Button } from "@edger/ui/components/ui/button";
import { Combobox } from "@edger/ui/components/ui/combobox";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
import {
  ChevronDownIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  ChevronsLeftIcon,
  ChevronsRightIcon,
  ChevronsUpDown,
  ChevronUpIcon,
} from "@edger/ui/icons/lucide";

export const DEFAULT_PAGE_SIZE = 15;
export const PAGE_SIZE_OPTIONS = [15, 30, 60] as const;

export function DataGrid<TData>({
  emptyText,
  onRowClick,
  rowLabel,
  table,
}: {
  emptyText: string;
  onRowClick?: (row: TData) => void;
  rowLabel?: (row: TData) => string;
  table: TanstackTable<TData>;
}) {
  return (
    <div className="flex w-full flex-col gap-2.5 overflow-auto">
      <div className="overflow-hidden rounded-md border">
        <Table>
          <TableHeader>
            {table.getHeaderGroups().map((headerGroup) => (
              <TableRow key={headerGroup.id}>
                {headerGroup.headers.map((header) => (
                  <TableHead key={header.id} colSpan={header.colSpan}>
                    {header.isPlaceholder
                      ? null
                      : flexRender(
                          header.column.columnDef.header,
                          header.getContext(),
                        )}
                  </TableHead>
                ))}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {table.getRowModel().rows.length > 0 ? (
              table.getRowModel().rows.map((row) => (
                <TableRow
                  aria-label={rowLabel?.(row.original)}
                  className={
                    onRowClick
                      ? "cursor-pointer focus-visible:bg-muted focus-visible:outline-none"
                      : undefined
                  }
                  key={row.id}
                  onClick={() => onRowClick?.(row.original)}
                  onKeyDown={(event) => {
                    if (!onRowClick || !["Enter", " "].includes(event.key)) {
                      return;
                    }
                    event.preventDefault();
                    onRowClick(row.original);
                  }}
                  tabIndex={onRowClick ? 0 : undefined}
                >
                  {row.getVisibleCells().map((cell) => (
                    <TableCell key={cell.id}>
                      {flexRender(
                        cell.column.columnDef.cell,
                        cell.getContext(),
                      )}
                    </TableCell>
                  ))}
                </TableRow>
              ))
            ) : (
              <TableRow>
                <TableCell
                  className="h-24 text-center text-muted-foreground"
                  colSpan={table.getAllColumns().length}
                >
                  {emptyText}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>
      <DataGridPagination table={table} />
    </div>
  );
}

export function DataGridColumnHeader<TData>({
  column,
  label,
}: {
  column: Column<TData, unknown>;
  label: string;
}) {
  const sorted = column.getIsSorted();
  return (
    <Button
      className="-ml-2 h-8"
      onClick={() => column.toggleSorting(sorted === "asc")}
      size="sm"
      variant="ghost"
    >
      {label}
      {sorted === "asc" ? (
        <ChevronUpIcon />
      ) : sorted === "desc" ? (
        <ChevronDownIcon />
      ) : (
        <ChevronsUpDown />
      )}
    </Button>
  );
}

function DataGridPagination<TData>({
  table,
}: {
  table: TanstackTable<TData>;
}) {
  const pageCount = Math.max(table.getPageCount(), 1);
  const pageIndex = table.getState().pagination.pageIndex;
  const pageSize = table.getState().pagination.pageSize;
  return (
    <PaginationControls
      canNextPage={table.getCanNextPage()}
      canPreviousPage={table.getCanPreviousPage()}
      onFirstPage={() => table.setPageIndex(0)}
      onLastPage={() => table.setPageIndex(pageCount - 1)}
      onNextPage={() => table.nextPage()}
      onPageSizeChange={(value) => table.setPageSize(value)}
      onPreviousPage={() => table.previousPage()}
      pageCount={pageCount}
      pageIndex={pageIndex}
      pageSize={pageSize}
    />
  );
}

export function PaginationControls({
  canNextPage,
  canPreviousPage,
  onFirstPage,
  onLastPage,
  onNextPage,
  onPageSizeChange,
  onPreviousPage,
  pageCount,
  pageIndex,
  pageSize,
}: {
  canNextPage: boolean;
  canPreviousPage: boolean;
  onFirstPage(): void;
  onLastPage(): void;
  onNextPage(): void;
  onPageSizeChange(value: number): void;
  onPreviousPage(): void;
  pageCount: number;
  pageIndex: number;
  pageSize: number;
}) {
  const sizeOptions = PAGE_SIZE_OPTIONS.map((value) => ({
    label: String(value),
    value: String(value),
  }));
  return (
    <div className="flex w-full flex-wrap items-center justify-end gap-3 overflow-auto p-1 sm:gap-4">
      <div className="font-medium text-sm">
        Page {pageIndex + 1} of {pageCount}
      </div>
      <div className="flex items-center gap-2">
        <Button
          aria-label="First page"
          className="hidden size-8 lg:inline-flex"
          disabled={!canPreviousPage}
          onClick={onFirstPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronsLeftIcon />
        </Button>
        <Button
          aria-label="Previous page"
          className="size-8"
          disabled={!canPreviousPage}
          onClick={onPreviousPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronLeftIcon />
        </Button>
        <Button
          aria-label="Next page"
          className="size-8"
          disabled={!canNextPage}
          onClick={onNextPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronRightIcon />
        </Button>
        <Button
          aria-label="Last page"
          className="hidden size-8 lg:inline-flex"
          disabled={!canNextPage}
          onClick={onLastPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronsRightIcon />
        </Button>
      </div>
      <Combobox
        aria-label="Rows per page"
        contentClassName="w-max! min-w-max! max-w-none!"
        onValueChange={(value) => onPageSizeChange(Number(value))}
        options={sizeOptions}
        searchable={false}
        side="top"
        triggerClassName="h-8 w-fit"
        value={String(pageSize)}
      />
    </div>
  );
}
