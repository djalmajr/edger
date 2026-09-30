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
import { useI18n } from "../lib/i18n";

export const DEFAULT_PAGE_SIZE = 15;
export const PAGE_SIZE_OPTIONS = [15, 30, 60] as const;

export function DataGrid<TData>({
  elasticColumnId,
  emptyText,
  fixedLayout = false,
  onRowClick,
  rowLabel,
  table,
}: {
  elasticColumnId?: string;
  emptyText: string;
  fixedLayout?: boolean;
  onRowClick?: (row: TData) => void;
  rowLabel?: (row: TData) => string;
  table: TanstackTable<TData>;
}) {
  // Opt-in elastic column: under the fixed layout it gets no explicit width,
  // so the browser assigns it everything the table width leaves over after
  // the px-defined columns. Non-opting grids keep the uniform distribution.
  const isElasticColumn = (column: Column<TData, unknown>) =>
    fixedLayout &&
    elasticColumnId !== undefined &&
    column.id === elasticColumnId;
  const fixedStyle = (column: Column<TData, unknown>) =>
    fixedLayout && !isElasticColumn(column)
      ? { width: column.getSize() }
      : undefined;
  return (
    <div className="flex w-full flex-col gap-2.5 overflow-auto">
      <div className="overflow-hidden rounded-md border">
        <Table
          style={
            fixedLayout
              ? {
                  tableLayout: "fixed",
                  // Sum only the visible leaf columns: a hidden column must
                  // not reserve width in the fixed layout.
                  minWidth: table
                    .getVisibleLeafColumns()
                    .reduce((sum, column) => sum + column.getSize(), 0),
                  width: "100%",
                }
              : undefined
          }
        >
          <TableHeader>
            {table.getHeaderGroups().map((headerGroup) => (
              <TableRow key={headerGroup.id}>
                {headerGroup.headers.map((header) => (
                  <TableHead
                    key={header.id}
                    colSpan={header.colSpan}
                    style={fixedStyle(header.column)}
                  >
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
                    <TableCell key={cell.id} style={fixedStyle(cell.column)}>
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
                  colSpan={table.getVisibleLeafColumns().length}
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
  const { t } = useI18n();
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
      labels={{
        firstPage: t("grid.firstPage"),
        lastPage: t("grid.lastPage"),
        nextPage: t("grid.nextPage"),
        pageSummary: t("grid.pageSummary")
          .replace("{page}", String(pageIndex + 1))
          .replace("{count}", String(pageCount)),
        previousPage: t("grid.previousPage"),
        rowsPerPage: t("grid.rowsPerPage"),
      }}
    />
  );
}

type PaginationLabels = {
  firstPage: string;
  lastPage: string;
  nextPage: string;
  pageSummary: string;
  previousPage: string;
  rowsPerPage: string;
};

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
  labels,
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
  labels?: PaginationLabels;
}) {
  const resolvedLabels: PaginationLabels = labels ?? {
    firstPage: "First page",
    lastPage: "Last page",
    nextPage: "Next page",
    pageSummary: `Page ${pageIndex + 1} of ${pageCount}`,
    previousPage: "Previous page",
    rowsPerPage: "Rows per page",
  };
  const sizeOptions = PAGE_SIZE_OPTIONS.map((value) => ({
    label: String(value),
    value: String(value),
  }));
  return (
    <div className="flex w-full flex-wrap items-center justify-end gap-3 overflow-auto p-1 sm:gap-4">
      <div className="font-medium text-sm">
        {resolvedLabels.pageSummary}
      </div>
      <div className="flex items-center gap-2">
        <Button
          aria-label={resolvedLabels.firstPage}
          className="hidden size-8 lg:inline-flex"
          disabled={!canPreviousPage}
          onClick={onFirstPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronsLeftIcon />
        </Button>
        <Button
          aria-label={resolvedLabels.previousPage}
          className="size-8"
          disabled={!canPreviousPage}
          onClick={onPreviousPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronLeftIcon />
        </Button>
        <Button
          aria-label={resolvedLabels.nextPage}
          className="size-8"
          disabled={!canNextPage}
          onClick={onNextPage}
          size="icon-sm"
          variant="outline"
        >
          <ChevronRightIcon />
        </Button>
        <Button
          aria-label={resolvedLabels.lastPage}
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
        aria-label={resolvedLabels.rowsPerPage}
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
