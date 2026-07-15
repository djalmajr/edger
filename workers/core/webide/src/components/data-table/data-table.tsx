import {
  flexRender,
  type Row,
  type Table as TanstackTable,
} from "@tanstack/react-table";
import type * as React from "react";

import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
import { cn } from "@edger/ui/lib/utils";

import { DataTablePagination } from "./data-table-pagination";

interface DataTableProps<TData> extends React.ComponentProps<"div"> {
  table: TanstackTable<TData>;
  emptyState?: React.ReactNode;
  onRowClick?: (row: Row<TData>) => void;
  paginated?: boolean;
}

export function DataTable<TData>({
  table,
  emptyState = "No results.",
  onRowClick,
  paginated = true,
  className,
  ...props
}: DataTableProps<TData>) {
  return (
    <div
      className={cn("flex w-full flex-col gap-2.5 overflow-auto", className)}
      {...props}
    >
      <div className="overflow-hidden rounded-md border bg-card">
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
            {table.getRowModel().rows.length ? (
              table.getRowModel().rows.map((row) => (
                <TableRow
                  className={cn(onRowClick && "cursor-pointer")}
                  data-state={row.getIsSelected() && "selected"}
                  key={row.id}
                  onClick={() => onRowClick?.(row)}
                  onKeyDown={(event) => {
                    if (
                      onRowClick &&
                      (event.key === "Enter" || event.key === " ")
                    ) {
                      event.preventDefault();
                      onRowClick(row);
                    }
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
                  {emptyState}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>
      {paginated ? <DataTablePagination table={table} /> : null}
    </div>
  );
}
