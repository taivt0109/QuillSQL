using System;
using System.IO;
using System.Runtime.Serialization;
using System.Runtime.Serialization.Json;
using System.Text;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Every user choice, in %LOCALAPPDATA%\QuillSQL\settings.json. Missing values take the
    /// defaults below, so an old or partial file keeps working.
    /// </summary>
    [DataContract]
    internal sealed class Settings
    {
        public static readonly string FilePath = Path.Combine(Updater.Folder, "settings.json");
        private static Settings _current;

        /// <summary>Raised on the UI thread after the settings are saved.</summary>
        public static event Action Changed;

        // General
        [DataMember(Name = "enabled")] public bool Enabled { get; set; }
        [DataMember(Name = "autoUpdate")] public bool AutoUpdate { get; set; }
        /// <summary>"en", "vi", or "auto" for the Windows language.</summary>
        [DataMember(Name = "language")] public string Language { get; set; }

        // Suggestions
        [DataMember(Name = "autoShowSuggestions")] public bool AutoShowSuggestions { get; set; }
        [DataMember(Name = "insertWithTab")] public bool InsertWithTab { get; set; }
        [DataMember(Name = "insertWithEnter")] public bool InsertWithEnter { get; set; }
        [DataMember(Name = "insertWithSpace")] public bool InsertWithSpace { get; set; }
        [DataMember(Name = "insertWithDot")] public bool InsertWithDot { get; set; }
        [DataMember(Name = "insertWithComma")] public bool InsertWithComma { get; set; }
        [DataMember(Name = "insertWithParenthesis")] public bool InsertWithParenthesis { get; set; }
        [DataMember(Name = "assignAliases")] public bool AssignAliases { get; set; }
        [DataMember(Name = "aliasWithAs")] public bool AliasWithAs { get; set; }
        [DataMember(Name = "qualifyObjectNames")] public bool QualifyObjectNames { get; set; }
        [DataMember(Name = "showSystemObjects")] public bool ShowSystemObjects { get; set; }
        [DataMember(Name = "showDefinitionBox")] public bool ShowDefinitionBox { get; set; }
        [DataMember(Name = "generateAlter")] public bool GenerateAlter { get; set; }

        // Typing
        [DataMember(Name = "closeSingleQuote")] public bool CloseSingleQuote { get; set; }
        [DataMember(Name = "closeSquareBracket")] public bool CloseSquareBracket { get; set; }
        [DataMember(Name = "closeParenthesis")] public bool CloseParenthesis { get; set; }
        [DataMember(Name = "closeComment")] public bool CloseComment { get; set; }
        [DataMember(Name = "overtypeClosing")] public bool OvertypeClosing { get; set; }

        // Editor
        [DataMember(Name = "objectCards")] public bool ObjectCards { get; set; }
        [DataMember(Name = "highlightMatches")] public bool HighlightMatches { get; set; }
        [DataMember(Name = "codeAnalysis")] public bool CodeAnalysis { get; set; }
        [DataMember(Name = "disabledRules")] public string[] DisabledRules { get; set; }

        // Running queries
        [DataMember(Name = "executionWarnings")] public bool ExecutionWarnings { get; set; }
        [DataMember(Name = "warnTruncate")] public bool WarnTruncate { get; set; }
        [DataMember(Name = "warnDrop")] public bool WarnDrop { get; set; }
        [DataMember(Name = "keepHistory")] public bool KeepHistory { get; set; }

        // Formatting extras
        [DataMember(Name = "formatExpandsWildcards")] public bool FormatExpandsWildcards { get; set; }
        [DataMember(Name = "formatQualifiesNames")] public bool FormatQualifiesNames { get; set; }

        public Settings() => SetDefaults();

        [OnDeserializing]
        private void OnDeserializing(StreamingContext context) => SetDefaults();

        private void SetDefaults()
        {
            Enabled = true;
            AutoUpdate = true;
            Language = "auto";
            AutoShowSuggestions = true;
            InsertWithTab = true;
            InsertWithEnter = true;
            InsertWithSpace = false;
            InsertWithDot = false;
            InsertWithComma = false;
            InsertWithParenthesis = false;
            AssignAliases = true;
            AliasWithAs = true;
            QualifyObjectNames = true;
            ShowSystemObjects = false;
            ShowDefinitionBox = true;
            GenerateAlter = true;
            CloseSingleQuote = true;
            CloseSquareBracket = true;
            CloseParenthesis = false;
            CloseComment = true;
            OvertypeClosing = true;
            ObjectCards = true;
            HighlightMatches = true;
            CodeAnalysis = true;
            DisabledRules = new string[0];
            ExecutionWarnings = true;
            WarnTruncate = false;
            WarnDrop = false;
            KeepHistory = true;
            FormatExpandsWildcards = false;
            FormatQualifiesNames = false;
        }

        public static Settings Current
        {
            get
            {
                if (_current == null)
                {
                    _current = Load();
                }
                return _current;
            }
        }

        private static Settings Load()
        {
            try
            {
                if (File.Exists(FilePath))
                {
                    var serializer = new DataContractJsonSerializer(typeof(Settings));
                    using (var stream = new MemoryStream(File.ReadAllBytes(FilePath)))
                    {
                        return serializer.ReadObject(stream) as Settings ?? new Settings();
                    }
                }
            }
            catch (Exception ex)
            {
                Log.Error("reading " + FilePath, ex);
            }
            return new Settings();
        }

        public Settings Clone()
        {
            var serializer = new DataContractJsonSerializer(typeof(Settings));
            using (var stream = new MemoryStream())
            {
                serializer.WriteObject(stream, this);
                stream.Position = 0;
                return (Settings)serializer.ReadObject(stream);
            }
        }

        public void Save()
        {
            Directory.CreateDirectory(Path.GetDirectoryName(FilePath));
            var serializer = new DataContractJsonSerializer(typeof(Settings), new DataContractJsonSerializerSettings { UseSimpleDictionaryFormat = true });
            using (var stream = new MemoryStream())
            using (var writer = JsonReaderWriterFactory.CreateJsonWriter(stream, Encoding.UTF8, false, true))
            {
                serializer.WriteObject(writer, this);
                writer.Flush();
                File.WriteAllBytes(FilePath, stream.ToArray());
            }
            _current = this;
            Changed?.Invoke();
        }

        /// <summary>The suggestion choices the engine applies, as a JSON object.</summary>
        public string CompletionOptionsJson() =>
            "{\"assignAliases\":" + Json.Bool(AssignAliases) + ",\"aliasWithAs\":" + Json.Bool(AliasWithAs)
            + ",\"qualifyObjectNames\":" + Json.Bool(QualifyObjectNames) + ",\"showSystemObjects\":" + Json.Bool(ShowSystemObjects) + "}";
    }
}
