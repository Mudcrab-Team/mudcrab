// Appended only to the SHA-verified temporary copy of the existing oracle.
// Keep the existing placed and placed-lo branches byte-for-byte intact.
static class MutagenP0Inspect
{
    public static int Run(string pluginPath)
    {
        var originalOut = Console.Out;
        using var incidentalOutput = new StringWriter(System.Globalization.CultureInfo.InvariantCulture);
        try
        {
            System.Globalization.CultureInfo.CurrentCulture = System.Globalization.CultureInfo.InvariantCulture;
            System.Globalization.CultureInfo.CurrentUICulture = System.Globalization.CultureInfo.InvariantCulture;

            var beforeBytes = File.ReadAllBytes(pluginPath);
            var beforeSha = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(beforeBytes)).ToLowerInvariant();
            Console.SetOut(incidentalOutput);

            // CreateFromBinary eagerly materializes Mutagen's mutable typed model.
            var mod = SkyrimMod.CreateFromBinary(pluginPath, SkyrimRelease.SkyrimSE);
            var records = new List<object>();
            var typedRecords = mod.EnumerateMajorRecords().ToList();
            foreach (var record in typedRecords)
            {
                var printed = new Noggog.StructuredStrings.StructuredStringBuilder();
                ((Noggog.StructuredStrings.IPrintable)record).Print(printed, "");

                var signature = FindSignature(record.GetType());
                var links = record.EnumerateFormLinks().Select(link => new
                {
                    target_form_key = link.FormKey.ToString(),
                    target_type = link.Type.FullName,
                    link_model_type = link.GetType().FullName,
                }).ToArray();
                object? localizedNameStringsKey = null;
                if (record is Mutagen.Bethesda.Skyrim.IArmorGetter localizedArmor)
                {
                    if (localizedArmor.Name is Mutagen.Bethesda.Strings.IOptionalStringsKeyGetter stringsKey)
                    {
                        localizedNameStringsKey = stringsKey.StringsKey.HasValue
                            ? new
                            {
                                state = "available",
                                value = (string?)System.Convert.ToString(stringsKey.StringsKey.Value, System.Globalization.CultureInfo.InvariantCulture),
                                source = "Mutagen IOptionalStringsKeyGetter",
                            }
                            : new
                            {
                                state = "unavailable",
                                value = (string?)null,
                                source = "Mutagen StringsKey is null",
                            };
                    }
                    else
                    {
                        localizedNameStringsKey = new
                        {
                            state = "unavailable",
                            value = (string?)null,
                            source = "typed Name does not expose IOptionalStringsKeyGetter",
                        };
                    }
                }

                records.Add(new
                {
                    signature = signature is null
                        ? new { state = "unavailable", value = (string?)null, source = "Mutagen typed registration" }
                        : new { state = "available", value = (string?)signature, source = "Mutagen typed registration" },
                    typed_registration = record.GetType().FullName,
                    form_key = record.FormKey.ToString(),
                    major_record_flags_raw = record.MajorRecordFlagsRaw,
                    editor_id = record.EditorID is null
                        ? new { state = "absent", value = (string?)null }
                        : new { state = "present", value = (string?)record.EditorID },
                    is_deleted = record.IsDeleted,
                    typed_data_print = new { state = "available", format = "Mutagen IPrintable", text = printed.ToString() },
                    localized_name_strings_key = localizedNameStringsKey,
                    typed_links = new { state = "available", order = "Mutagen enumeration order", items = links },
                    physical_offset = new { state = "unavailable", reason = "typed model does not expose physical offsets" },
                    physical_subrecord_order = new { state = "unavailable", reason = "typed model does not expose source subrecord order" },
                    unknown_payloads = new { state = "unavailable", reason = "typed model does not expose opaque source payloads" },
                });
            }

            var afterBytes = File.ReadAllBytes(pluginPath);
            var afterSha = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(afterBytes)).ToLowerInvariant();
            if (!StringComparer.Ordinal.Equals(beforeSha, afterSha))
                throw new InvalidDataException("input plugin changed while Mutagen was reading it");

            Console.SetOut(originalOut);
            var report = new
            {
                execution_status = "completed",
                acceptance_verdict = new { state = "unavailable", reason = "P0 typed observations do not qualify structural or catalog completeness" },
                adapter = new
                {
                    package = "Mutagen.Bethesda.Skyrim",
                    package_version = "0.54.4",
                    release = "SkyrimSE",
                    import = "SkyrimMod.CreateFromBinary",
                    current_culture = "InvariantCulture",
                },
                source_plugin = new
                {
                    file_name = Path.GetFileName(pluginPath),
                    mod_key = mod.ModKey.ToString(),
                    sha256 = beforeSha,
                    size_bytes = beforeBytes.LongLength,
                    is_master = mod.IsMaster,
                    is_small_master = mod.IsSmallMaster,
                    using_localization = mod.UsingLocalization,
                },
                diagnostics = new
                {
                    captured_stdout = incidentalOutput.ToString(),
                },
                typed_major_record_count = records.Count,
                source_occurrence_coverage = new
                {
                    state = "unavailable",
                    reason = "typed group enumeration may collapse or skip physical source occurrences",
                },
                framing_observations = new
                {
                    offsets = "unavailable",
                    subrecord_order = "unavailable",
                    unknown_payloads = "unavailable",
                },
                records,
            };
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(report));
            return 0;
        }
        catch (Exception ex)
        {
            Console.SetOut(originalOut);
            var captured = incidentalOutput.ToString();
            if (!String.IsNullOrEmpty(captured))
                Console.Error.WriteLine($"Mutagen stdout before failure: {captured}");
            Console.Error.WriteLine($"Mutagen inspect failed: {ex.GetType().FullName}: {ex.Message}");
            return 2;
        }
        finally
        {
            Console.SetOut(originalOut);
        }
    }

    private static string? FindSignature(Type recordType)
    {
        for (var type = recordType; type is not null; type = type.BaseType)
        {
            var field = type.GetField(
                "GrupRecordType",
                System.Reflection.BindingFlags.Public
                    | System.Reflection.BindingFlags.NonPublic
                    | System.Reflection.BindingFlags.Static
                    | System.Reflection.BindingFlags.DeclaredOnly);
            if (field?.GetValue(null) is { } recordTypeValue)
                return recordTypeValue.ToString();
        }
        return null;
    }
}
