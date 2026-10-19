// Pass 5: runtime name pointers for the remaining 41f230 slots; xrefs for the
// fStaggerMin / fStaggerBlockAttackShieldMult value slots.
// Args: <out_dir>
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSpace;
import java.io.File;
import java.io.PrintWriter;

public class StaggerRecon5 extends GhidraScript {
    @Override
    public void run() throws Exception {
        String outDir = getScriptArgs()[0];
        new File(outDir).mkdirs();
        long base = currentProgram.getImageBase().getOffset();
        AddressSpace space = currentProgram.getAddressFactory().getDefaultAddressSpace();

        long[] structs = {0x1420a60a0L, 0x1420a6118L, 0x1420a6130L, 0x1420a6148L, 0x1420a6160L,
                          0x1420a6190L, 0x1420a6208L, 0x1420a6088L, 0x1420a7cd8L, 0x1420a03d8L,
                          0x1420a5d88L, 0x1420a6070L, 0x1420a7cc0L};
        try (PrintWriter w = new PrintWriter(new File(outDir, "names5.tsv"), "UTF-8")) {
            for (long va : structs) {
                String name = "<no ptr>";
                int val = 0;
                try {
                    long q = currentProgram.getMemory().getLong(space.getAddress(va));
                    if (q > 0x140000000L && q < 0x148000000L) {
                        byte[] b = new byte[80];
                        currentProgram.getMemory().getBytes(space.getAddress(q), b);
                        int len = 0;
                        while (len < b.length && b[len] != 0) len++;
                        if (len > 1 && len < 80) name = new String(b, 0, len, "UTF-8");
                    }
                    val = currentProgram.getMemory().getInt(space.getAddress(va + 0x10));
                } catch (Exception e) { /* band */ }
                w.printf("%x\t%s\t%08x\t%f%n", va, name, val, Float.intBitsToFloat(val));
            }
        }

        String[] slots = {"1420a7ce8", "1420a6218", "1420a6098"};
        for (String s : slots) {
            Address a = space.getAddress(Long.parseUnsignedLong(s, 16));
            try (PrintWriter w = new PrintWriter(new File(outDir, "xs_" + s + ".tsv"), "UTF-8")) {
                for (var r : currentProgram.getReferenceManager().getReferencesTo(a)) {
                    Address from = r.getFromAddress();
                    var f = getFunctionContaining(from);
                    w.println(from + "\t" + (f != null ? f.getEntryPoint().toString() : "NONE"));
                }
            }
        }
        println("STAGGER-RECON5-DONE");
    }
}
