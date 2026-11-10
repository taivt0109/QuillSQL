using System;
using System.Collections.Generic;
using System.Linq;
using System.Reflection;
using System.Reflection.Emit;
using System.Runtime.CompilerServices;
using System.Text.RegularExpressions;
using System.Windows.Threading;
using Microsoft.VisualStudio.Shell;

namespace QuillSql.Vsix
{
    /// <summary>
    /// Adds "QuillSQL: Tab Color..." to the right-click menu of servers and databases in Object
    /// Explorer, the Compare commands to databases and "Document with AI" to tables and views. Object
    /// Explorer's menus take items derived from its ToolsMenuItemBase class; SSMS's
    /// assemblies are not available when QuillSQL is built, so the item type is made at run time
    /// and everything else goes through reflection. If any of it is missing, the menu item simply
    /// does not appear; the command in the QuillSQL menu does the same for the active window.
    /// </summary>
    internal static class ObjectExplorerMenu
    {
        private const string Ns = "Microsoft.SqlServer.Management.UI.VSIntegration.ObjectExplorer.";
        private static readonly ConditionalWeakTable<object, object> Extended = new ConditionalWeakTable<object, object>();
        private static DispatcherTimer _retry;
        private static object _explorer;
        private static Type _itemType, _schemaItemType, _dataItemType, _documentItemType, _explorerType, _nodeType, _contextType, _menuHandlerType;
        private static ModuleBuilder _module;

        /// <summary>The package, for opening Compare windows from the menu.</summary>
        internal static AsyncPackage HostPackage;
        private static int _attempts;

        public static void Start()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            // Object Explorer loads after the shell; try again until it is there.
            _retry = new DispatcherTimer { Interval = TimeSpan.FromSeconds(3) };
            _retry.Tick += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                if (TryHook() || ++_attempts > 60)
                {
                    _retry.Stop();
                }
            };
            _retry.Start();
        }

        private static Type FindType(string fullName) =>
            AppDomain.CurrentDomain.GetAssemblies().Select(a => a.GetType(fullName, false)).FirstOrDefault(t => t != null);

        private static bool TryHook()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                _explorerType = FindType(Ns + "IObjectExplorerService");
                var contextServiceType = FindType("Microsoft.SqlServer.Management.IContextService");
                if (_explorerType == null || contextServiceType == null)
                {
                    return false;
                }
                _explorer = Package.GetGlobalService(_explorerType);
                if (!(_explorer is IServiceProvider provider) || !(provider.GetService(contextServiceType) is object contextService))
                {
                    return false;
                }
                var actionContext = ConnectionResolver.Get(contextService, "ActionContext");
                var changed = actionContext?.GetType().GetEvent("CurrentContextChanged");
                if (changed == null)
                {
                    return false;
                }
                _nodeType = FindType(Ns + "INodeInformation");
                _contextType = FindType(Ns + "INodeContext");
                _menuHandlerType = FindType(Ns + "IMenuHandler");
                var itemBase = FindType(Ns + "ToolsMenuItemBase");
                _itemType = MakeItemType(itemBase, "QuillSql.TabColorMenuItem", typeof(ObjectExplorerCallbacks));
                _schemaItemType = MakeItemType(itemBase, "QuillSql.CompareSchemaMenuItem", typeof(CompareSchemaCallbacks));
                _dataItemType = MakeItemType(itemBase, "QuillSql.CompareDataMenuItem", typeof(CompareDataCallbacks));
                _documentItemType = MakeItemType(itemBase, "QuillSql.DocumentMenuItem", typeof(DocumentCallbacks));
                changed.AddEventHandler(actionContext, new EventHandler(OnSelectionChanged));
                Log.Write("Object Explorer menu ready");
                return true;
            }
            catch (Exception ex)
            {
                Log.Error("adding to the Object Explorer menu", ex);
                return true;
            }
        }

        /// <summary>A subclass of ToolsMenuItemBase whose Invoke and Clone call `callbacks`' static methods.</summary>
        private static Type MakeItemType(Type itemBase, string name, Type callbacks)
        {
            if (_module == null)
            {
                var assembly = AppDomain.CurrentDomain.DefineDynamicAssembly(new AssemblyName("QuillSql.ObjectExplorerItems"), AssemblyBuilderAccess.Run);
                _module = assembly.DefineDynamicModule("Items");
            }
            var type = _module.DefineType(name, TypeAttributes.Public | TypeAttributes.Class | TypeAttributes.Sealed, itemBase);
            const BindingFlags Members = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic;

            var baseConstructor = itemBase.GetConstructor(Members, null, Type.EmptyTypes, null);
            var constructor = type.DefineConstructor(MethodAttributes.Public, CallingConventions.Standard, Type.EmptyTypes);
            var il = constructor.GetILGenerator();
            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Call, baseConstructor);
            il.Emit(OpCodes.Ret);

            void forward(string name, Type returns, string callback)
            {
                var overridden = itemBase.GetMethod(name, Members, null, Type.EmptyTypes, null);
                var access = overridden.Attributes & MethodAttributes.MemberAccessMask;
                var method = type.DefineMethod(name, access | MethodAttributes.Virtual | MethodAttributes.HideBySig, returns, Type.EmptyTypes);
                var body = method.GetILGenerator();
                body.Emit(OpCodes.Ldarg_0);
                body.Emit(OpCodes.Call, callbacks.GetMethod(callback));
                body.Emit(OpCodes.Ret);
                type.DefineMethodOverride(method, overridden);
            }
            forward("Invoke", typeof(void), nameof(ObjectExplorerCallbacks.Invoke));
            forward("Clone", typeof(object), nameof(ObjectExplorerCallbacks.Clone));
            return type.CreateType();
        }

        public static object NewItem() => Item(_itemType, "QuillSQL: Tab Color...");

        public static object NewSchemaItem() => Item(_schemaItemType, "QuillSQL: Compare Schema With...");

        public static object NewDataItem() => Item(_dataItemType, "QuillSQL: Compare Data With...");

        public static object NewDocumentItem() => Item(_documentItemType, "QuillSQL: Document with AI...");

        private static object Item(Type type, string text)
        {
            var item = Activator.CreateInstance(type);
            type.GetProperty("Text")?.SetValue(item, Loc.T(text));
            return item;
        }

        /// <summary>The connection of the node a menu item belongs to.</summary>
        public static object Connection(object item)
        {
            var node = item.GetType().GetProperty("Parent")?.GetValue(item);
            return node == null ? null : ConnectionResolver.Get(node, "Connection");
        }

        private static void OnSelectionChanged(object sender, EventArgs e)
        {
            try
            {
                var arguments = new object[] { 0, null };
                _explorerType.GetMethod("GetSelectedNodes").Invoke(_explorer, arguments);
                if (!(arguments[1] is Array nodes) || nodes.Length != 1)
                {
                    return;
                }
                var node = nodes.GetValue(0);
                var path = _contextType.GetProperty("UrnPath")?.GetValue(node) as string;
                Log.Verbose("Object Explorer selection: " + path);
                var objectNode = path == "Server/Database/Table" || path == "Server/Database/View";
                if (path != "Server" && path != "Server/Database" && !objectNode)
                {
                    return;
                }
                if (!(node is IServiceProvider provider) || !(provider.GetService(_menuHandlerType) is object menu) || Extended.TryGetValue(menu, out _))
                {
                    return;
                }
                Extended.Add(menu, null);
                var addChild = menu.GetType().GetMethod("AddChild", new[] { typeof(string), typeof(object) });
                if (objectNode)
                {
                    addChild?.Invoke(menu, new object[] { string.Empty, NewDocumentItem() });
                    return;
                }
                addChild?.Invoke(menu, new object[] { string.Empty, NewItem() });
                if (path == "Server/Database" && _schemaItemType != null)
                {
                    addChild?.Invoke(menu, new object[] { string.Empty, NewSchemaItem() });
                    addChild?.Invoke(menu, new object[] { string.Empty, NewDataItem() });
                }
                Log.Verbose("tab color item added to the " + path + " menu");
            }
            catch (Exception ex)
            {
                Log.Error("Object Explorer selection", ex);
            }
        }

        /// <summary>The server and database of an Object Explorer node, from its context path.</summary>
        public static Tuple<string, string> Names(object item)
        {
            var node = item.GetType().GetProperty("Parent")?.GetValue(item);
            var context = node == null ? null : _contextType.GetProperty("Context")?.GetValue(node) as string;
            if (context == null)
            {
                return null;
            }
            string part(string kind)
            {
                var m = Regex.Match(context, kind + @"\[@Name='((?:[^']|'')*)'");
                return m.Success ? m.Groups[1].Value.Replace("''", "'") : null;
            }
            var server = part("Server");
            return server == null ? null : Tuple.Create(server, part("Database"));
        }

        /// <summary>The schema.name of a table or view node, from its context path.</summary>
        public static string ObjectName(object item)
        {
            var node = item.GetType().GetProperty("Parent")?.GetValue(item);
            var context = node == null ? null : _contextType.GetProperty("Context")?.GetValue(node) as string;
            var m = context == null ? null : Regex.Match(context, @"(?:Table|View)\[@Name='((?:[^']|'')*)' and @Schema='((?:[^']|'')*)'\]");
            return m == null || !m.Success ? null : m.Groups[2].Value.Replace("''", "'") + "." + m.Groups[1].Value.Replace("''", "'");
        }
    }

    /// <summary>Called by the menu item type made at run time; public so that type may call it.</summary>
    public static class ObjectExplorerCallbacks
    {
        public static void Invoke(object item)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var names = ObjectExplorerMenu.Names(item);
                if (names == null)
                {
                    EditorContext.Status("could not read the server of this node.");
                    return;
                }
                // The rule must name the server as query windows describe it: their connection's
                // ServerName (what was typed in Connect, e.g. myserver.database.windows.net or an
                // alias), not the node's URN name, which is the server's own @@SERVERNAME
                // (e.g. myserver) and so never found in a window's "server / database".
                var connection = ObjectExplorerMenu.Connection(item);
                var server = connection == null ? null : ConnectionResolver.Get(connection, "ServerName") as string;
                TabColorDialog.Show(string.IsNullOrEmpty(server) ? names.Item1 : server, names.Item2);
            }
            catch (Exception ex)
            {
                Log.Error("tab color from Object Explorer", ex);
            }
        }

        public static object Clone(object item) => ObjectExplorerMenu.NewItem();
    }

    /// <summary>
    /// "QuillSQL: Document with AI..." on a table or view: a new query window on its database, where
    /// the script that sets the descriptions Claude writes appears, to review and run.
    /// </summary>
    public static class DocumentCallbacks
    {
        public static void Invoke(object item)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var name = ObjectExplorerMenu.ObjectName(item);
                if (name == null || ObjectExplorerMenu.HostPackage == null)
                {
                    EditorContext.Status("could not read the name of this table.");
                    return;
                }
                // New Query connects to the database selected in Object Explorer.
                EditorContext.Dte?.ExecuteCommand("File.NewQuery");
                var target = ConnectionResolver.TryGetActive();
                var view = EditorContext.Active()?.View;
                if (target == null || !target.CanQuery || view == null)
                {
                    EditorContext.Status("could not open a query window on this database.");
                    return;
                }
                EditorContext.For(view)?.Replace(0, 0, "-- " + Loc.T("Claude is describing " + name + "...") + Environment.NewLine);
                AiEdits.DocumentTable(ObjectExplorerMenu.HostPackage, target, name, script =>
                {
                    ThreadHelper.ThrowIfNotOnUIThread();
                    var editor = EditorContext.For(view);
                    if (editor != null)
                    {
                        editor.ReplaceAll(script);
                    }
                    else
                    {
                        EditorContext.OpenInNewQuery(script);
                    }
                });
            }
            catch (Exception ex)
            {
                Log.Error("document with AI from Object Explorer", ex);
            }
        }

        public static object Clone(object item) => ObjectExplorerMenu.NewDocumentItem();
    }

    /// <summary>"QuillSQL: Compare Schema With..." on a database: a Compare window with it as the source.</summary>
    public static class CompareSchemaCallbacks
    {
        public static void Invoke(object item) => CompareCallbacks.Open(item, false);

        public static object Clone(object item) => ObjectExplorerMenu.NewSchemaItem();
    }

    /// <summary>"QuillSQL: Compare Data With..." on a database.</summary>
    public static class CompareDataCallbacks
    {
        public static void Invoke(object item) => CompareCallbacks.Open(item, true);

        public static object Clone(object item) => ObjectExplorerMenu.NewDataItem();
    }

    internal static class CompareCallbacks
    {
        public static void Open(object item, bool data)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var names = ObjectExplorerMenu.Names(item);
                var connection = ObjectExplorerMenu.Connection(item);
                string password = null;
                var source = connection == null || names == null ? null : ComparePane.SourceFrom(connection, names.Item2, out password);
                if (ObjectExplorerMenu.HostPackage != null)
                {
                    ComparePane.Open(ObjectExplorerMenu.HostPackage, data, source, password);
                }
            }
            catch (Exception ex)
            {
                Log.Error("compare from Object Explorer", ex);
            }
        }
    }
}
