// Args: <out.txt> <symbol-regex> [vtableSlotHexOffset]
// Prints matching symbols; for vftable symbols, the function address at the
// given byte offset into the vtable.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Symbol;
import ghidra.program.model.symbol.SymbolIterator;
import java.io.PrintWriter;
import java.util.regex.Pattern;

public class SymbolVtable extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        long base = currentProgram.getImageBase().getOffset();
        Pattern pattern = Pattern.compile(args[1]);
        long slot = args.length > 2 ? Long.parseLong(args[2], 16) : -1;
        try (PrintWriter out = new PrintWriter(args[0], "UTF-8")) {
            SymbolIterator symbols = currentProgram.getSymbolTable().getAllSymbols(true);
            while (symbols.hasNext()) {
                Symbol s = symbols.next();
                if (!pattern.matcher(s.getName()).find()) {
                    continue;
                }
                Address a = s.getAddress();
                Function f = getFunctionAt(a);
                String fn = f == null ? "" : " func=" + (f.getEntryPoint().getOffset() - base);
                out.printf("%s\t%s\t%x%s%n", s.getName(), s.getParentNamespace().getName(),
                    a.getOffset() - base, fn);
                if (slot >= 0 && s.getName().contains("vftable")) {
                    Address target = toAddr(a.getOffset() + slot);
                    long value = getLong(target);
                    Function slotFn = getFunctionAt(toAddr(value));
                    out.printf("  slot+%x -> %x%s%n", slot, value - base,
                        slotFn == null ? " (no function)" :
                            " func=" + (slotFn.getEntryPoint().getOffset() - base));
                }
            }
        }
    }
}
