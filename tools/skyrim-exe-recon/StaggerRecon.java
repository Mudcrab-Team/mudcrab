// Stagger-composition closure: decompile the FUN_1407f0690 helper family,
// the 0x6ca640 consumer, and the stagger-entry sites; xref the entry points;
// dump every pool/setting constant the decompiles touch.
// Args: <out_dir>
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSpace;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;
import java.io.File;
import java.io.PrintWriter;

public class StaggerRecon extends GhidraScript {
    private void decompile(DecompInterface di, long base, String rva, File out) throws Exception {
        long r = Long.parseUnsignedLong(rva, 16);
        Function f = getFunctionContaining(toAddr(base + r));
        if (f == null) { println("no function at " + rva); return; }
        DecompileResults res = di.decompileFunction(f, 240, monitor);
        try (PrintWriter w = new PrintWriter(new File(out, rva + ".c"), "UTF-8")) {
            w.println(res.decompileCompleted() ? res.getDecompiledFunction().getC()
                                               : "// failed: " + res.getErrorMessage());
        }
    }

    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        String outDir = args[0];
        File out = new File(outDir);
        out.mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();

        String[][] groups = {
            {"dec", "41f5e0", "41f230", "41f560", "28c140", "6aef60", "6cba10", "69fc00",
             "69fb20", "679d00", "6cecb0", "21b950", "720660", "6ca640", "6797c0", "7f0690",
             "6cd2f0"},
        };
        DecompInterface di = new DecompInterface();
        di.openProgram(currentProgram);
        for (String[] g : groups) {
            new File(outDir, g[0]).mkdirs();
            for (int i = 1; i < g.length; i++) decompile(di, base, g[i], new File(outDir, g[0]));
        }
        di.dispose();

        // xrefs to function starts: all callers, one tsv per target
        String[] xrefTargets = {"1406ca640", "14069fc00", "14069fb20", "1406ca230",
                                "14041f560", "14041f5e0", "14041f230", "1407f0690",
                                "1406cba10", "1406aef60", "140679d00"};
        for (String t : xrefTargets) {
            Address a = space.getAddress(Long.parseUnsignedLong(t, 16));
            try (PrintWriter w = new PrintWriter(new File(out, "x_" + t + ".tsv"), "UTF-8")) {
                for (Reference r : currentProgram.getReferenceManager().getReferencesTo(a)) {
                    Address from = r.getFromAddress();
                    Function f = getFunctionContaining(from);
                    w.println(from + "\t" + (f != null ? f.getEntryPoint().toString() : "NONE"));
                }
            }
        }

        // pool / setting value slots: hex u32 and float
        String[] memVAs = {"141b5bf14", "1417f7cf0", "1420a5d98", "1420a5d80", "1420a72e0",
                           "1420a72f8", "1420a7cd0", "1420a6080", "14219e55c", "141b5bdd0",
                           "1417ed698", "1417ed6c0", "1420b6a18", "1420b6e68",
                           // name-pointer slots (value - 0x10) for candidate settings
                           "1420a7cc0", "1420a6070", "1420a5d88", "1420a5d70", "1420a72d0",
                           "14219e54c"};
        try (PrintWriter w = new PrintWriter(new File(out, "mem.tsv"), "UTF-8")) {
            for (String s : memVAs) {
                long va = Long.parseUnsignedLong(s, 16);
                try {
                    Address a = space.getAddress(va);
                    long q = currentProgram.getMemory().getLong(a);
                    int u = currentProgram.getMemory().getInt(a);
                    float f = Float.intBitsToFloat(u);
                    // try to follow a plausible name pointer (qlook of q as string)
                    String name = "";
                    if (q > 0x140000000L && q < 0x144000000L) {
                        try {
                            Address na = space.getAddress(q);
                            byte[] b = new byte[64];
                            currentProgram.getMemory().getBytes(na, b);
                            int len = 0;
                            while (len < b.length && b[len] != 0) len++;
                            if (len > 0 && len < 64) name = new String(b, 0, len, "UTF-8");
                        } catch (Exception e2) { /* not a string */ }
                    }
                    w.printf("%s\tq=%016x\tu=%08x\tf=%f\tname=%s%n", s, q, u, f, name);
                } catch (Exception e) {
                    w.printf("%s\tERR\t%s%n", s, e.getMessage());
                }
            }
        }
        println("STAGGER-RECON-DONE");
    }
}
