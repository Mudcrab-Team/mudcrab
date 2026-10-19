// Writes "entry_rva\tmin_rva\tmax_rva_exclusive\tname" for every function to the path in arg 0.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressRange;
import ghidra.program.model.listing.Function;
import java.io.PrintWriter;

public class ExportFunctionBounds extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length != 1) {
            throw new IllegalArgumentException("usage: ExportFunctionBounds <out.tsv>");
        }
        long base = currentProgram.getImageBase().getOffset();
        try (PrintWriter out = new PrintWriter(args[0], "UTF-8")) {
            for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
                Address entry = f.getEntryPoint();
                long lo = Long.MAX_VALUE;
                long hi = Long.MIN_VALUE;
                for (AddressRange r : f.getBody()) {
                    lo = Math.min(lo, r.getMinAddress().getOffset());
                    hi = Math.max(hi, r.getMaxAddress().getOffset() + 1);
                }
                out.printf("%x\t%x\t%x\t%s%n", entry.getOffset() - base, lo - base, hi - base, f.getName());
            }
        }
        println("Exported " + currentProgram.getFunctionManager().getFunctionCount() + " functions");
    }
}
