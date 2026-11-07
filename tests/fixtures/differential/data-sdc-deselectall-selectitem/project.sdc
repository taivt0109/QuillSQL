<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<!--
SQL Data Compare
SQL Data Compare
Version:15.4.17.28422-->
<Project version="5" type="SQLComparisonToolsProject">
  <DataSource1 version="5" type="LiveDatabaseSource">
    <ServerName>{server}</ServerName>
    <DatabaseName>{db1}</DatabaseName>
    <Username>{user}</Username>
    <SavePassword>False</SavePassword>
    <EncryptConnection>False</EncryptConnection>
    <TrustServerCertificate>True</TrustServerCertificate>
    <Password />
    <ScriptFolderLocation />
    <MigrationsFolderLocation />
    <AuthenticationType>SqlPassword</AuthenticationType>
  </DataSource1>
  <DataSource2 version="5" type="LiveDatabaseSource">
    <ServerName>{server}</ServerName>
    <DatabaseName>{db2}</DatabaseName>
    <Username>{user}</Username>
    <SavePassword>False</SavePassword>
    <EncryptConnection>False</EncryptConnection>
    <TrustServerCertificate>True</TrustServerCertificate>
    <Password />
    <ScriptFolderLocation />
    <MigrationsFolderLocation />
    <AuthenticationType>SqlPassword</AuthenticationType>
  </DataSource2>
  <LastCompared>01/01/2026 00:00:00</LastCompared>
  <Options>Default</Options>
  <InRecycleBin>False</InRecycleBin>
  <Direction>0</Direction>
  <ProjectFilter version="1" type="DifferenceFilter">
    <FilterCaseSensitive>False</FilterCaseSensitive>
    <Filters version="1">
      <None version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </None>
      <Assembly version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Assembly>
      <AsymmetricKey version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </AsymmetricKey>
      <Certificate version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Certificate>
      <Contract version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Contract>
      <DdlTrigger version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </DdlTrigger>
      <Default version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Default>
      <ExtendedProperty version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </ExtendedProperty>
      <EventNotification version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </EventNotification>
      <FullTextCatalog version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </FullTextCatalog>
      <FullTextStoplist version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </FullTextStoplist>
      <Function version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Function>
      <MessageType version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </MessageType>
      <PartitionFunction version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </PartitionFunction>
      <PartitionScheme version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </PartitionScheme>
      <Queue version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Queue>
      <Role version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Role>
      <Route version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Route>
      <Rule version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Rule>
      <Schema version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Schema>
      <SearchPropertyList version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </SearchPropertyList>
      <SecurityPolicy version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </SecurityPolicy>
      <Sequence version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Sequence>
      <Service version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Service>
      <ServiceBinding version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </ServiceBinding>
      <StoredProcedure version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </StoredProcedure>
      <SymmetricKey version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </SymmetricKey>
      <Synonym version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Synonym>
      <Table version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </Table>
      <User version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </User>
      <UserDefinedType version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </UserDefinedType>
      <View version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </View>
      <XmlSchemaCollection version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </XmlSchemaCollection>
      <ExternalFileFormat version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </ExternalFileFormat>
      <ExternalDataSource version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </ExternalDataSource>
      <ExternalTable version="1">
        <Include>True</Include>
        <Expression>TRUE</Expression>
      </ExternalTable>
    </Filters>
  </ProjectFilter>
  <ProjectFilterName />
  <UserNote />
  <SelectedSyncObjects version="1" type="SelectedSyncObjects">
    <Schemas type="ListString" version="2" />
    <Grouping type="ListByte" version="2">
      <value type="Byte">0</value>
      <value type="Byte">0</value>
      <value type="Byte">0</value>
      <value type="Byte">0</value>
      <value type="Byte">0</value>
      <value type="Byte">0</value>
    </Grouping>
    <SelectAll>False</SelectAll>
  </SelectedSyncObjects>
  <SCGroupingStyle>0</SCGroupingStyle>
  <SQLOptions>10</SQLOptions>
  <MappingOptions>82</MappingOptions>
  <ComparisonOptions>0</ComparisonOptions>
  <TableActions type="ArrayList" version="1">
    <value version="1" type="SelectTableEvent">
      <action>DeselectAll</action>
    </value>
    <value version="1" type="SelectTableEvent">
      <action>SelectItem</action>
      <val>[dbo].[A]:[dbo].[A]</val>
    </value>
  </TableActions>
  <SessionSettings>14</SessionSettings>
  <DCGroupingStyle>0</DCGroupingStyle>
  <SC_DeploymentOptions version="1" type="SC_DeploymentOptions">
    <BackupOptions version="1" type="BackupOptions">
      <BackupProvider>Native</BackupProvider>
      <TypeOfBackup>Full</TypeOfBackup>
      <Folder />
      <Filename />
      <SqbLicenseType>None</SqbLicenseType>
      <SqbVersion>0</SqbVersion>
      <DefaultNativeFolder />
      <DefaultSqbFolder />
      <Password encrypted="1" />
      <NameFileAutomatically>False</NameFileAutomatically>
      <OverwriteIfExists>False</OverwriteIfExists>
      <CompressionLevel>0</CompressionLevel>
      <EncryptionLevel>None</EncryptionLevel>
      <ThreadCount>0</ThreadCount>
      <BackupEnabled>False</BackupEnabled>
    </BackupOptions>
  </SC_DeploymentOptions>
</Project>
