// Pass 3: remaining helper decompiles, xref census of every settings slot the
// stagger chain reads, weapon-class pool values, runtime name-pointer dump.
// Args: <out_dir>
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSpace;
import ghidra.program.model.listing.Function;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import java.io.File;
import java.io.PrintWriter;

public class StaggerRecon3 extends GhidraScript {
    @Override
    public void run() throws Exception {
        String outDir = getScriptArgs()[0];
        File out = new File(outDir);
        out.mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();

        String[] fns = {"6742a0", "6ca140", "5535a0", "6ca230", "5e2d30", "643490"};
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        for (String r : fns) {
            Function f = getFunctionContaining(space.getAddress(base + Long.parseUnsignedLong(r, 16)));
            if (f == null) { println("nofn " + r); continue; }
            DecompileResults res = di.decompileFunction(f, 240, monitor);
            try (PrintWriter w = new PrintWriter(new File(out, r + ".c"), "UTF-8")) {
                w.println(res.decompileCompleted() ? res.getDecompiledFunction().getC()
                                                   : "// failed: " + res.getErrorMessage());
            }
        }
        di.dispose();

        String[] xrefSlots = {"1420a6080", "1420a7cd0", "1420a6110", "1420a60f8", "1420a7e98",
                              "1420a7eb0", "1420a7ec8", "1420a6200", "1420a61e8", "1420a60e0",
                              "1420a60c8", "1420a6098", "1420a60b0", "1420a6128", "1420a6140",
                              "1420a6158", "1420a6170", "1420a6188", "1420a61a0", "141b5be28",
                              "14207b488", "14207b4a0", "14207b4b8", "14207b4d0"};
        for (String s : xrefSlots) {
            Address a = space.getAddress(Long.parseUnsignedLong(s, 16));
            try (PrintWriter w = new PrintWriter(new File(out, "xs_" + s + ".tsv"), "UTF-8")) {
                for (var r : currentProgram.getReferenceManager().getReferencesTo(a)) {
                    Address from = r.getFromAddress();
                    Function f = getFunctionContaining(from);
                    w.println(from + "\t" + (f != null ? f.getEntryPoint().toString() : "NONE"));
                }
            }
        }

        String[] structVAs = {"1420a6090", "1420a60a8", "1420a60b8", "1420a60d0", "1420a60e8",
                              "1420a6100", "1420a6120", "1420a6138", "1420a6150", "1420a6168",
                              "1420a6180", "1420a6198", "1420a61d8", "1420a61f0", "1420a7e88",
                              "1420a7ea0", "1420a7eb8", "1420a7cc0"};
        try (PrintWriter w = new PrintWriter(new File(out, "names.tsv"), "UTF-8")) {
            for (String s : structVAs) {
                long va = Long.parseUnsignedLong(s, 16);
                String name = "<no ptr>";
                try {
                    long q = currentProgram.getMemory().getLong(space.getAddress(va));
                    if (q > 0x140000000L && q < 0x148000000L) {
                        byte[] b = new byte[80];
                        currentProgram.getMemory().getBytes(space.getAddress(q), b);
                        int len = 0;
                        while (len < b.length && b[len] != 0) len++;
                        if (len > 0 && len < 80) name = new String(b, 0, len, "UTF-8");
                    }
                } catch (Exception e) { /* unreadable band */ }
                w.println(s + "\t" + name);
            }
        }
        println("STAGGER-RECON3-DONE");
    }
}
