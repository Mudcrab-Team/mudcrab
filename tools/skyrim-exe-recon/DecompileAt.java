// Args: <out_dir> <hex rva>... Decompiles the function at each RVA to <out_dir>/<rva>.c.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import java.io.File;
import java.io.PrintWriter;

public class DecompileAt extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        File out = new File(args[0]);
        out.mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        for (int i = 1; i < args.length; i++) {
            long rva = Long.parseUnsignedLong(args[i], 16);
            Function f = getFunctionContaining(toAddr(base + rva));
            if (f == null) {
                println("no function at " + args[i]);
                continue;
            }
            DecompileResults r = di.decompileFunction(f, 120, monitor);
            try (PrintWriter w = new PrintWriter(new File(out, args[i] + ".c"), "UTF-8")) {
                w.println(r.decompileCompleted() ? r.getDecompiledFunction().getC() : "// failed: " + r.getErrorMessage());
            }
        }
        di.dispose();
    }
}
