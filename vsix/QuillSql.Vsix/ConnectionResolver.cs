using System;
using System.Collections.Specialized;
using System.Data;
using Microsoft.Data.SqlClient;
using System.Linq;
using System.Reflection;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;

namespace QuillSql.Vsix
{
    internal sealed class ConnectionTarget
    {
        public string Key { get; set; }
        public string ConnectionString { get; set; }
        public string Description { get; set; }

        /// <summary>False for logins QuillSQL cannot open a connection with, such as Microsoft Entra.</summary>
        public bool CanQuery => ConnectionString != null;
    }

    /// <summary>
    /// Finds the connection of the active query window. SSMS exposes it only through
    /// non-public members of the editor's query executor, so every step is defensive.
    /// </summary>
    internal static class ConnectionResolver
    {
        private const BindingFlags Any = BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic;
        private static bool _loggedShape;

        public static ConnectionTarget TryGetActive()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            try
            {
                var monitor = Package.GetGlobalService(typeof(SVsShellMonitorSelection)) as IVsMonitorSelection;
                if (monitor == null
                    || monitor.GetCurrentElementValue((uint)VSConstants.VSSELELEMID.SEID_DocumentFrame, out var frameObject) != VSConstants.S_OK
                    || !(frameObject is IVsWindowFrame frame)
                    || frame.GetProperty((int)__VSFPROPID.VSFPROPID_DocView, out var docView) != VSConstants.S_OK)
                {
                    return null;
                }
                return ForDocView(docView, true);
            }
            catch (Exception ex)
            {
                Log.Error("resolving connection", ex);
                return null;
            }
        }

        /// <summary>The connection of one query window's document view, or null.</summary>
        public static ConnectionTarget ForDocView(object docView, bool logShape = false)
        {
            try
            {
                // SSMS 21: the query editor control exposes its connection directly.
                var info = First(Get(docView, "ConnectionInfoList"));
                var connection = (Get(docView, "Connection") ?? Get(docView, "m_connection")) as IDbConnection;
                if (info == null && connection == null)
                {
                    // Older SSMS: through the query executor's connection strategy.
                    var strategy = Get(Get(docView, "QueryExecutor"), "ConnectionStrategy");
                    info = Get(strategy, "_connectionInfo") ?? Get(strategy, "ConnectionInfo") ?? Get(strategy, "UIConnectionInfo");
                    connection = Get(strategy, "Connection") as IDbConnection;
                }
                if (info == null && connection == null)
                {
                    if (logShape && !_loggedShape)
                    {
                        _loggedShape = true;
                        Log.Write("no connection on " + Describe(docView));
                    }
                    return null;
                }
                return Build(info, connection, logShape);
            }
            catch (Exception ex)
            {
                Log.Error("resolving connection", ex);
                return null;
            }
        }

        private static ConnectionTarget Build(object info, IDbConnection connection, bool log)
        {
            var server = Get(info, "ServerName") as string;
            if (string.IsNullOrEmpty(server))
            {
                server = (connection as System.Data.Common.DbConnection)?.DataSource
                    ?? (connection == null ? null : new SqlConnectionStringBuilder(connection.ConnectionString).DataSource);
            }
            if (string.IsNullOrEmpty(server))
            {
                return null;
            }
            var options = Get(info, "AdvancedOptions") as NameValueCollection;
            var database = connection?.Database;
            if (string.IsNullOrEmpty(database))
            {
                database = options?["DATABASE"];
            }

            var builder = new SqlConnectionStringBuilder
            {
                DataSource = server,
                ApplicationName = "QuillSQL IntelliSense",
                ConnectTimeout = 15,
            };
            if (!string.IsNullOrEmpty(database))
            {
                builder.InitialCatalog = database;
            }
            var encryption = options?["ENCRYPT_CONNECTION"];
            if (string.Equals(encryption, "Strict", StringComparison.OrdinalIgnoreCase))
            {
                builder.Encrypt = SqlConnectionEncryptOption.Strict;
            }
            else if (bool.TryParse(encryption, out var encrypt))
            {
                builder.Encrypt = encrypt;
            }
            if (bool.TryParse(options?["TRUST_SERVER_CERTIFICATE"], out var trust))
            {
                builder.TrustServerCertificate = trust;
            }

            // SSMS: 0 = Windows, 1 = SQL Server; other values are Microsoft Entra methods.
            var authentication = info == null ? 0 : Convert.ToInt32(Get(info, "AuthenticationType") ?? 0);
            var user = Get(info, "UserName") as string;
            switch (authentication)
            {
                case 0:
                    builder.IntegratedSecurity = true;
                    break;
                case 1:
                    builder.UserID = user;
                    builder.Password = Get(info, "Password") as string ?? "";
                    break;
                default:
                    if (log)
                    {
                        Log.Write("authentication type " + authentication + " is not supported for metadata; suggestions use the script only");
                    }
                    // The window is still connected; callers that only need its name get one.
                    return new ConnectionTarget
                    {
                        Key = "",
                        ConnectionString = null,
                        Description = server + " / " + (database ?? "(default)"),
                    };
            }

            return new ConnectionTarget
            {
                Key = string.Join("|", server, database ?? "", authentication, user ?? ""),
                ConnectionString = builder.ConnectionString,
                Description = server + " / " + (database ?? "(default)"),
            };
        }

        /// <summary>The first element of a sequence, or the value itself when it is not a sequence.</summary>
        private static object First(object value)
        {
            if (value is System.Collections.IEnumerable items && !(value is string))
            {
                foreach (var item in items)
                {
                    return item;
                }
                return null;
            }
            return value;
        }

        /// <summary>Property or field value by name, searching non-public members and base types.</summary>
        public static object Get(object target, string name)
        {
            for (var type = target?.GetType(); type != null; type = type.BaseType)
            {
                var property = type.GetProperty(name, Any | BindingFlags.DeclaredOnly);
                if (property != null && property.GetIndexParameters().Length == 0)
                {
                    return property.GetValue(target);
                }
                var field = type.GetField(name, Any | BindingFlags.DeclaredOnly);
                if (field != null)
                {
                    return field.GetValue(target);
                }
            }
            return null;
        }

        private static string Describe(object target)
        {
            if (target == null)
            {
                return "null";
            }
            var type = target.GetType();
            var members = type.GetProperties(Any).Select(p => p.Name).Concat(type.GetFields(Any).Select(f => f.Name));
            return type.FullName + " {" + string.Join(", ", members) + "}";
        }
    }
}
