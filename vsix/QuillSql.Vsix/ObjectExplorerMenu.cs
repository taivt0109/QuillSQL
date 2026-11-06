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
    /// Explorer. Object Explorer's menus take items derived from its ToolsMenuItemBase class; SSMS's
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
        private static Type _itemType, _explorerType, _nodeType, _contextType, _menuHandlerType;
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
                _itemType = MakeItemType(FindType(Ns + "ToolsMenuItemBase"));
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

        /// <summary>A subclass of ToolsMenuItemBase whose Invoke and Clone call back into QuillSQL.</summary>
        private static Type MakeItemType(Type itemBase)
        {
            var assembly = AppDomain.CurrentDomain.DefineDynamicAssembly(new AssemblyName("QuillSql.ObjectExplorerItems"), AssemblyBuilderAccess.Run);
            var type = assembly.DefineDynamicModule("Items").DefineType("QuillSql.TabColorMenuItem", TypeAttributes.Public | TypeAttributes.Class | TypeAttributes.Sealed, itemBase);
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
                body.Emit(OpCodes.Call, typeof(ObjectExplorerCallbacks).GetMethod(callback));
                body.Emit(OpCodes.Ret);
                type.DefineMethodOverride(method, overridden);
            }
            forward("Invoke", typeof(void), nameof(ObjectExplorerCallbacks.Invoke));
            forward("Clone", typeof(object), nameof(ObjectExplorerCallbacks.Clone));
            return type.CreateType();
        }

        public static object NewItem()
        {
            var item = Activator.CreateInstance(_itemType);
            _itemType.GetProperty("Text")?.SetValue(item, Loc.T("QuillSQL: Tab Color..."));
            return item;
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
                if (path != "Server" && path != "Server/Database")
                {
                    return;
                }
                if (!(node is IServiceProvider provider) || !(provider.GetService(_menuHandlerType) is object menu) || Extended.TryGetValue(menu, out _))
                {
                    return;
                }
                Extended.Add(menu, null);
                menu.GetType().GetMethod("AddChild", new[] { typeof(string), typeof(object) })?.Invoke(menu, new object[] { string.Empty, NewItem() });
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
                TabColorDialog.Show(names.Item1, names.Item2);
            }
            catch (Exception ex)
            {
                Log.Error("tab color from Object Explorer", ex);
            }
        }

        public static object Clone(object item) => ObjectExplorerMenu.NewItem();
    }
}
